//! "Copy Python" of a pin or an ROI: the script that reads the series Janus shows with Zeit itself (`load_raster`:
//! the dates, the bands, the window; the few lines of xarray for what Zeit has no function for: a normalized
//! difference, a quality band's rule, the units of the reflectance) and makes the same Zeit calls with the same
//! parameters. The app's chart and the website's guides both call `pixel_script`.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use janus_cube::{CubeInfo, QaRule};

use super::{ToolCall, py_float, tool_call, wrap_call};
use crate::pixel::{Params, PixelInput};

/// How the script's reads are cut into blocks: a block of a few hundred cells a side holds all the bands of a
/// date-after-date stack (CCDC's 875), and `load_raster(..., clip=)` then reads one block, not the file.
const CHUNKS: &str = "{\"x\": 256, \"y\": 256}";

/// The ROI as the chart has it.
#[derive(Clone, Debug, Default)]
pub struct Roi {
    /// the window of the files' pixels, the end exclusive
    pub x0: usize,
    pub y0: usize,
    pub x1: usize,
    pub y1: usize,
    /// read subsampled (Janus does above 4 Mpx): the grid it was read at
    pub sampled: Option<(usize, usize)>,
    /// the polygon's rings in the files' pixels, each `[x0, y0, x1, y1...]`; none for a rectangle
    pub rings: Vec<Vec<f64>>,
}

/// What the script is about.
#[derive(Clone, Debug)]
pub enum Target {
    /// the pin `id`, on the column and row of the files
    Pin {
        id: u32,
        col: i64,
        row: i64,
    },
    Roi(Roi),
    /// a raster run: the whole window of the files' pixels (the end exclusive), no pin or mean out of it; the maps
    /// of the results are written with `save_raster` under `out_dir`
    Run {
        window: [usize; 4],
        out_dir: String,
        /// what the script reproduces ("the run of Mann-Kendall")
        what: String,
    },
}

/// One step of a Time series analysis chain, parallel to `Spec::tools`.
#[derive(Clone, Debug, Default)]
pub struct Step {
    /// the step's folder name (`01_interpolate`), the name of the maps it writes
    pub name: String,
    /// the series cannot take the step (why): only a comment of it is written
    pub skipped: Option<String>,
    /// the step reads the analysis series (what an earlier Interpolate or Standardize left), not the series
    pub reads_analysis: bool,
    /// the step's first result is the analysis series the steps after it read
    pub leaves_analysis: bool,
}

/// What `pixel_script` needs.
pub struct Spec<'a> {
    pub info: &'a CubeInfo,
    /// Janus' version, for the docstring
    pub version: &'a str,
    /// the layer's name
    pub name: &'a str,
    pub target: Target,
    /// the band of each role (blue, green, red, nir, swir1, swir2, thermal) in a date's bands
    pub roles: &'a BTreeMap<String, i32>,
    /// the tools fitted on the chart and their parameters
    pub tools: &'a [(String, Params)],
    /// a file's path as the script writes it (the app: its absolute path; the website's guides: beside the script)
    pub path: &'a dyn Fn(&str) -> String,
    /// the second series a tool reads (ImproPhe's coarse one), over its whole grid
    pub other: Option<&'a CubeInfo>,
    /// a Time series analysis chain: how each of `tools` is placed in it (empty: the tools are not a chain)
    pub chain: &'a [Step],
}

/// A Python string literal (paths with backslashes, names with quotes); the paths with forward slashes, which
/// Python reads on every system.
fn dq(s: &str) -> String {
    let s = s.replace('\\', "/");
    format!("\"{}\"", s.replace('"', "\\\""))
}

fn num(v: f64) -> String {
    janus_cube::multidim::format_g(v, 12)
}

/// The scale and offset GDAL gives a band of a file (1, 0 where it has none).
fn scale_offset(path: &str, band: i32) -> (f64, f64) {
    let Ok(ds) = gdal::Dataset::open(janus_cube::gdal_path(path)) else { return (1.0, 0.0) };
    let Ok(rb) = ds.rasterband(band.max(1) as usize) else { return (1.0, 0.0) };
    (rb.scale().unwrap_or(1.0), rb.offset().unwrap_or(0.0))
}

/// Whether Zeit reads the file's bands as dates times one named band (a `(time, band, y, x)` cube of one band, as
/// `EVI_2001_01_01` is the date 2001-01-01 of the band EVI), not as plain dates, from the dates the file carries.
fn dates_with_a_band_name(path: &str) -> bool {
    use gdal::Metadata as _;
    let Ok(ds) = gdal::Dataset::open(janus_cube::gdal_path(path)) else { return false };
    if ds.metadata_item("ZEIT_TIME", "").is_some() {
        return false; // the dates save_raster wrote come first
    }
    let n = ds.raster_count();
    let labels: Vec<String> =
        (1..=n).map(|b| ds.rasterband(b).map(|r| r.description().unwrap_or_default()).unwrap_or_default()).collect();
    !labels.is_empty() && labels.iter().all(|l| date_band_label(l))
}

/// A label that is a date and a name (`EVI_2001_01_01`, `2020-01-15_red`), not only a date (`yr1985`, `2020-01-15`).
fn date_band_label(label: &str) -> bool {
    let t = label.trim();
    let b = t.as_bytes();
    let digits = |from: usize, n: usize| from + n <= b.len() && b[from..from + n].iter().all(u8::is_ascii_digit);
    let sep = |i: usize| i < b.len() && matches!(b[i], b'-' | b'_' | b'.');
    // a label that is only a year, with its prefix
    let only_year = {
        let rest = t.trim_start_matches(|c: char| c.is_ascii_alphabetic()).trim_start_matches(['_', '-', ' ']);
        rest.len() == 4 && rest.bytes().all(|c| c.is_ascii_digit()) && t.len() - rest.len() <= 5
    };
    if t.is_empty() || only_year {
        return false;
    }
    // the first date in it: its length
    let mut i = 0;
    while i < b.len() {
        let before = i > 0 && b[i - 1].is_ascii_digit();
        let len = if before {
            0
        } else if digits(i, 4) && sep(i + 4) && digits(i + 5, 2) && sep(i + 7) && digits(i + 8, 2) && !digits(i + 10, 1)
        {
            10
        } else if digits(i, 8) && !digits(i + 8, 1) {
            8
        } else if digits(i, 7) && !digits(i + 7, 1) {
            7
        } else if digits(i, 4) && !digits(i + 4, 1) {
            4
        } else {
            0
        };
        if len > 0 {
            let rest = format!("{}{}", &t[..i], &t[i + len..]);
            return !rest.trim_matches(['_', '-', ' ', '.']).is_empty();
        }
        i += 1;
    }
    false
}

/// `v * scale + offset` as a statement's right side, if it changes anything.
fn scaled(var: &str, (scale, offset): (f64, f64)) -> Option<String> {
    if scale == 1.0 && offset == 0.0 {
        return None;
    }
    let mut e = var.to_string();
    if scale != 1.0 {
        e = format!("{e} * {}", py_float(scale));
    }
    if offset != 0.0 {
        e = format!("{e} {} {}", if offset < 0.0 { '-' } else { '+' }, py_float(offset.abs()));
    }
    Some(e)
}

/// `var` with each band's scale and offset (`band` the dimension), if any changes something.
fn scaled_bands(var: &str, so: &[(f64, f64)]) -> Option<String> {
    if so.iter().all(|s| *s == so[0]) {
        return scaled(var, so[0]).map(|e| format!("{var} = {e}  # the files' scale and offset"));
    }
    let list = |f: &dyn Fn(&(f64, f64)) -> f64| {
        format!("[{}]", so.iter().map(|s| py_float(f(s))).collect::<Vec<_>>().join(", "))
    };
    Some(format!(
        "{var} = {var} * xr.DataArray({}, dims=\"band\") + xr.DataArray({}, dims=\"band\")  # each band's scale and offset in its file",
        list(&|s| s.0),
        list(&|s| s.1)
    ))
}

/// Which observations a quality band keeps, as xarray over `qa` (NaN: no value, not kept).
fn usable(rule: QaRule) -> &'static str {
    match rule {
        QaRule::Fmask => "qa.isin([0, 1])",
        QaRule::LandsatC2 => "(qa.fillna(1).astype(\"int64\") & 0x3B) == 0",
        QaRule::Scl => "qa.isin([4, 5, 6])",
        QaRule::HlsFmask => "(qa.fillna(255).astype(\"int64\") & 0x1E) == 0",
        QaRule::NonZero => "qa > 0",
        QaRule::None => "qa.notnull()",
    }
}

/// The Fmask code (0 clear, 1 water, 2 shadow, 3 snow, 4 cloud, 255 no observation) of the quality band's code `c`.
fn fmask_of(rule: QaRule) -> &'static str {
    match rule {
        QaRule::LandsatC2 => {
            "xr.where((c & 1) > 0, 255, xr.where((c & 10) > 0, 4, xr.where((c & 16) > 0, 2, \
             xr.where((c & 32) > 0, 3, xr.where((c & 128) > 0, 1, 0)))))"
        }
        QaRule::HlsFmask => {
            "xr.where(c == 255, 255, xr.where((c & 6) > 0, 4, xr.where((c & 8) > 0, 2, \
             xr.where((c & 16) > 0, 3, xr.where((c & 32) > 0, 1, 0)))))"
        }
        QaRule::Scl => {
            "xr.where(c.isin([4, 5]), 0, xr.where(c == 6, 1, xr.where(c.isin([2, 3]), 2, \
             xr.where(c == 11, 3, xr.where(c.isin([7, 8, 9, 10]), 4, 255)))))"
        }
        QaRule::NonZero => "xr.where(c != 0, 0, 4)",
        QaRule::Fmask | QaRule::None => "c",
    }
}

/// The lines of the script.
#[derive(Default)]
struct Code(Vec<String>);

impl Code {
    fn line(&mut self, s: impl Into<String>) {
        self.0.push(s.into());
    }

    fn blank(&mut self) {
        if self.0.last().is_some_and(|l| !l.is_empty()) {
            self.0.push(String::new());
        }
    }

    fn text(&self) -> String {
        self.0.join("\n")
    }
}

/// The shown series over a window, or the pieces of it, as the script names them.
struct Reads {
    /// the Python expression that is the files (a path, a folder, a list of paths)
    src: String,
    /// one file with the dates as bands, else a file a date
    stack: bool,
    /// the bands of a date's file
    bpd: usize,
    /// an argument of every read (a NetCDF / Zarr variable's name)
    extra: Option<String>,
    /// the CRS Janus assumes for a lon/lat grid without one (a NetCDF), which Zeit's `clip=` needs to be told
    assumed_crs: Option<String>,
}

/// The files of the series as `load_raster` takes them: the path of the one file, the folder when the series is
/// all the rasters of it, else the list.
fn reads(info: &CubeInfo, path: &dyn Fn(&str) -> String) -> Result<Reads, String> {
    let bpd = info.bands_per_date.max(1);
    let first = &info.layers[0].path;
    let stack = info.layers.iter().all(|l| &l.path == first);
    if stack {
        let file = janus_cube::dataset_file(first);
        let extra = if info.multidim && !info.md_variable.is_empty() {
            if !info.md_levels.is_empty() {
                return Err("a NetCDF / Zarr variable with levels is not in Copy Python yet".into());
            }
            Some(format!("band={}", dq(&info.md_variable)))
        } else {
            None
        };
        let assumed_crs = (info.multidim && info.crs_name.contains("assumed")).then(|| info.crs_authority.clone());
        return Ok(Reads { src: dq(&path(file)), stack: true, bpd, extra, assumed_crs });
    }
    // a file a date
    let written: Vec<String> = info.layers.iter().map(|l| path(&l.path).replace('\\', "/")).collect();
    let parent = |p: &str| p.rfind('/').map_or(String::new(), |i| p[..i].to_string());
    let folder = parent(&written[0]);
    let same_folder = written.iter().all(|w| parent(w) == folder)
        && info
            .layers
            .iter()
            .all(|l| std::path::Path::new(&l.path).parent() == std::path::Path::new(&info.layers[0].path).parent());
    if same_folder && let Some(dir) = std::path::Path::new(&info.layers[0].path).parent() {
        // Zeit reads every raster of a folder: that is the series if no other is there
        let exts = [".tif", ".tiff", ".vrt", ".img", ".jp2", ".hdf", ".h5", ".asc"];
        let mut there: Vec<String> = std::fs::read_dir(dir)
            .map(|d| {
                d.filter_map(Result::ok)
                    .filter(|e| e.path().is_file())
                    .map(|e| e.path().to_string_lossy().into_owned())
                    .filter(|p| exts.iter().any(|x| p.to_lowercase().ends_with(x)))
                    .collect()
            })
            .unwrap_or_default();
        let mut ours: Vec<String> =
            info.layers.iter().map(|l| std::path::Path::new(&l.path).to_string_lossy().into_owned()).collect();
        there.sort();
        ours.sort();
        if there == ours {
            let f = if folder.is_empty() { ".".to_string() } else { folder };
            return Ok(Reads { src: dq(&f), stack: false, bpd, extra: None, assumed_crs: None });
        }
    }
    let list: Vec<String> = written.iter().map(|w| format!("    {},", dq(w))).collect();
    Ok(Reads { src: format!("[\n{}\n]", list.join("\n")), stack: false, bpd, extra: None, assumed_crs: None })
}

/// The window the script reads, and how it takes the pin or the ROI out of it.
struct Window {
    /// the `clip=` argument, in the files' coordinates
    clip: Option<String>,
    /// polygon rings, written before the reads
    polygon: Option<String>,
    /// a comment on what the window is
    note: String,
}

fn window(spec: &Spec) -> Result<Window, String> {
    let info = spec.info;
    let g = info.geo_transform.unwrap_or([0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
    if g[2] != 0.0 || g[4] != 0.0 {
        return Err("a rotated raster is not in Copy Python".into());
    }
    let at = |px: f64, py: f64| (g[0] + px * g[1], g[3] + py * g[5]);
    let bounds = |x0: f64, y0: f64, x1: f64, y1: f64| {
        let (ax, ay) = at(x0, y0);
        let (bx, by) = at(x1, y1);
        format!("({}, {}, {}, {})", num(ax.min(bx)), num(ay.min(by)), num(ax.max(bx)), num(ay.max(by)))
    };
    let crs = if info.crs_authority.is_empty() { info.crs_name.as_str() } else { info.crs_authority.as_str() };
    Ok(match &spec.target {
        Target::Run { window: [x0, y0, x1, y1], .. } => {
            let (w, h) = (x1 - x0, y1 - y0);
            let whole = (*x0, *y0, *x1, *y1) == (0, 0, info.width, info.height);
            let note = if whole {
                format!("The run's window: the whole raster ({w} x {h} pixels)")
            } else {
                format!("The run's window: x {x0}..{x1}, y {y0}..{y1} of the files' pixels ({w} x {h})")
            };
            let (x0, y0, x1, y1) = (*x0 as f64, *y0 as f64, *x1 as f64, *y1 as f64);
            let clip = (!whole).then(|| bounds(x0 + 0.4, y0 + 0.4, x1 - 0.4, y1 - 0.4));
            Window { clip, polygon: None, note }
        }
        Target::Pin { id, col, row } => {
            let (cx, cy) = at(*col as f64 + 0.5, *row as f64 + 0.5);
            let mut note = format!("Pin {id}: column {col}, row {row} of the files");
            if info.geo_transform.is_some() {
                let _ = write!(note, ", its centre at x {}, y {}", num(cx), num(cy));
                if !crs.is_empty() {
                    let _ = write!(note, " ({crs})");
                }
            }
            Window { clip: None, polygon: None, note }
        }
        Target::Roi(roi) => {
            let (w, h) = (roi.x1 - roi.x0, roi.y1 - roi.y0);
            let mut note =
                format!("The ROI: x {}..{}, y {}..{} of the files' pixels ({w} x {h})", roi.x0, roi.x1, roi.y0, roi.y1);
            if let Some((bw, bh)) = roi.sampled {
                let _ = write!(
                    note,
                    ".\n# Janus read it at {bw} x {bh} (nearest neighbour, above 4 Mpx); every pixel is used here, so the mean differs slightly"
                );
            }
            if roi.rings.is_empty() {
                let (x0, y0, x1, y1) = (roi.x0 as f64, roi.y0 as f64, roi.x1 as f64, roi.y1 as f64);
                Window { clip: Some(bounds(x0 + 0.4, y0 + 0.4, x1 - 0.4, y1 - 0.4)), polygon: None, note }
            } else {
                // the rings in the files' coordinates, even-odd like Janus' (a hole is a ring inside another)
                let rings: Vec<String> = roi
                    .rings
                    .iter()
                    .map(|r| {
                        let mut pts: Vec<(f64, f64)> = r.as_chunks::<2>().0.iter().map(|v| at(v[0], v[1])).collect();
                        if let Some(&f) = pts.first() {
                            pts.push(f);
                        }
                        let p: Vec<String> = pts.iter().map(|(x, y)| format!("({}, {})", num(*x), num(*y))).collect();
                        format!("[{}]", p.join(", "))
                    })
                    .collect();
                note += "; its polygon, a cell counting when its centre is inside";
                Window {
                    clip: Some("[polygon]".into()),
                    polygon: Some(format!(
                        "polygon = {{\"type\": \"Polygon\", \"coordinates\": [{}]}}",
                        rings.join(",\n    ")
                    )),
                    note,
                }
            }
        }
    })
}

/// A Python list of ints.
fn ints(v: &[i32]) -> String {
    format!("[{}]", v.iter().map(i32::to_string).collect::<Vec<_>>().join(", "))
}

fn strs(v: &[&str]) -> String {
    format!("[{}]", v.iter().map(|s| format!("\"{s}\"")).collect::<Vec<_>>().join(", "))
}

/// The Python of a pin's or an ROI's series (and the tools fitted on it).
pub fn pixel_script(spec: &Spec) -> Result<String, String> {
    let info = spec.info;
    if info.layers.is_empty() {
        return Err("the series has no dates".into());
    }
    if info.stack {
        return Err("a stack of different variables is not a series".into());
    }
    let t_n = info.t();

    // the tools' calls
    let roles_empty: BTreeMap<String, Vec<f64>> = spec.roles.keys().map(|r| (r.clone(), vec![])).collect();
    let mut input = PixelInput {
        years: (0..t_n).map(|t| info.decimal_year(t)).collect(),
        days: info.time_is_date.then(|| (0..t_n).map(|t| info.unix_day(t)).collect()),
        values: vec![0.0; t_n],
        bands: roles_empty,
        ..Default::default()
    };
    let (run, roi) = (matches!(spec.target, Target::Run { .. }), matches!(spec.target, Target::Roi(_)));
    // the Zeit calls, their position among the chain's steps (`sequence`: a call, or the note of a step left out)
    let mut calls: Vec<(String, Params, ToolCall)> = vec![];
    let mut sequence: Vec<Result<usize, String>> = vec![];
    let mut left_out: Vec<String> = vec![];
    if !spec.chain.is_empty() && spec.chain.len() != spec.tools.len() {
        return Err("the chain's steps are not the tools".into());
    }
    for (k, (id, params)) in spec.tools.iter().enumerate() {
        let tool = crate::manifest().tool(id).ok_or_else(|| format!("no tool {id}"))?;
        let step = spec.chain.get(k);
        if let Some(why) = step.and_then(|s| s.skipped.as_ref()) {
            sequence.push(Err(format!("# {}. {}: left out ({why})", k + 1, tool.name)));
            continue;
        }
        // each date's sensor and granule metadata, for the tools that need them (found in the files, as the run does)
        input.sensors = if crate::sensors::needs(id) { crate::sensors::of_series(info) } else { vec![] };
        input.granules = if crate::granules::needs(id, params) { crate::granules::of_series(info) } else { vec![] };
        // a tool the Zeit calls cannot be written for (it needs dates the series lacks, or endmembers) is left out
        let note = match tool_call(id, params, &input) {
            // the bands of a date are one pixel's: an ROI has the mean of its series only
            Some(c) if roi && c.inputs != ["series"] => {
                format!("# {}: its bands are a pin's, not the ROI's mean", tool.name)
            }
            // the two series ImproPhe reads are a run's
            Some(c) if c.inputs.contains(&"improphe_cubes") && !run => {
                format!("# {}: it needs the whole window of a raster run", tool.name)
            }
            Some(c) => {
                let c = match step {
                    Some(s) if s.reads_analysis => c.reading("analysis"),
                    _ => c,
                };
                sequence.push(Ok(calls.len()));
                calls.push((tool.name.clone(), params.clone(), c));
                continue;
            }
            None => format!("# {}: Zeit's call cannot be written for this series", tool.name),
        };
        if step.is_some() {
            sequence.push(Err(note));
        } else {
            left_out.push(note);
        }
    }
    if run && calls.is_empty() && spec.chain.is_empty() {
        return Err(left_out.first().map_or("no tool to run".into(), |l| l.trim_start_matches("# ").to_string()));
    }
    let has = |input: &str| calls.iter().any(|(_, _, c)| c.inputs.contains(&input));
    let want_bands = has("bands_cube");
    let want_all = has("bands_all");
    let want_spectrum = has("spectrum");
    let want_nbar = has("nbar_cube");
    let want_improphe = has("improphe_cubes");
    let want_series = calls.is_empty() || has("series") || want_all || want_spectrum;
    if info.spectral && (want_bands || want_all || want_nbar) {
        return Err("a tool of the bands of a date needs a series of dates".into());
    }

    let rd = reads(info, spec.path)?;
    let win = window(spec)?;
    if win.clip.is_some() && rd.assumed_crs.is_none() && info.crs_wkt.is_empty() && info.crs_name.is_empty() {
        return Err("the files have no CRS: Zeit's load_raster cannot cut a window out of them".into());
    }
    let sel = info.sel;
    let qa_on = sel.qa_band > 0 && sel.qa_rule != QaRule::None;
    let first = &info.layers[0];

    if !info.time_is_date && !info.spectral && !(rd.stack && rd.bpd == 1) {
        return Err("a series without dates is not in Copy Python: it needs the dates Zeit reads".into());
    }

    // ---- what the reads are named, and how many there are
    // a read of band `n` of a date (1-based in the date's bands)
    let n_reads = {
        let mut n = 0;
        if want_series {
            n += 1 + usize::from(sel.nd_band > 0 && rd.bpd > 1);
        }
        if qa_on && (want_series || want_bands || want_improphe) {
            n += 1;
        }
        if want_bands {
            n += 1;
        }
        n
    };
    let stack_cube = rd.stack && rd.bpd > 1;
    // a run reads its window whole, the chart's series by blocks
    let chunks = if run { String::new() } else { format!(", chunks={CHUNKS}") };
    let region_opts =
        format!("masked=True{chunks}{}", win.clip.as_ref().map_or(String::new(), |c| format!(", clip={c}")));
    // one read is written whole where it is used; several share `src` and `region`
    let single_read = n_reads <= 1
        && !stack_cube
        && !want_bands
        && !want_all
        && !want_nbar
        && !want_improphe
        && info.expr.is_none()
        && !rd.src.starts_with('[');

    let mut c = Code::default();
    let mut body = Code::default();
    // the arguments of a read of band `band` of the dates' files (None: the file as it is)
    let read_args = |band: Option<i32>| -> Vec<String> {
        let mut a = vec![if single_read { rd.src.clone() } else { "src".to_string() }];
        a.extend(rd.extra.clone());
        if let Some(b) = band.filter(|_| rd.bpd > 1) {
            a.push(format!("band={b}"));
        }
        if single_read {
            a.push("masked=True".into());
            if !run {
                a.push(format!("chunks={CHUNKS}"));
            }
            a.extend(win.clip.as_ref().map(|c| format!("clip={c}")));
        } else {
            a.push("**region".into());
        }
        a
    };
    // `var = <what reads the band>`: a read, or a band of the stack's cube
    let read_stmt = |var: &str, band: Option<i32>, comment: &str| -> String {
        let text = if stack_cube && band.is_some() {
            format!("{var} = cube.isel(band={})", band.unwrap_or(1) - 1)
        } else if let (Some(crs), Some(clip)) = (&rd.assumed_crs, &win.clip) {
            // the file has no CRS: say it, then cut the window
            let cut = if win.polygon.is_some() {
                "clip([polygon], drop=True)".to_string()
            } else {
                format!("clip_box({})", clip.trim_start_matches('(').trim_end_matches(')'))
            };
            let extra = rd.extra.as_ref().map_or(String::new(), |e| format!(", {e}"));
            format!(
                "{var} = zeit.load_raster({}{extra}, masked=True)\n{var} = {var}.rio.write_crs(\"{crs}\").rio.{cut}",
                rd.src
            )
        } else if single_read {
            wrap_call(var, "zeit.load_raster", &read_args(band))
        } else {
            format!("{var} = zeit.load_raster({})", read_args(band).join(", "))
        };
        if comment.is_empty() { text } else { format!("{text}  # {comment}") }
    };
    body.line(format!("# {}", win.note));
    if let Some(p) = &win.polygon {
        body.line(p);
    }
    if !single_read {
        body.line(format!("src = {}", rd.src));
        body.line(format!("region = dict({region_opts})"));
    }
    if stack_cube {
        let extra = rd.extra.as_ref().map_or(String::new(), |e| format!(", {e}"));
        body.line(format!(
            "cube = zeit.load_raster(src{extra}, **region)  # (time, band, y, x): the bands of each date"
        ));
    }
    let name_of =
        |n: i32| -> String { info.band_names.get(n as usize - 1).cloned().unwrap_or_else(|| format!("band {n}")) };

    // ---- the series Janus shows
    if want_series {
        if rd.stack && rd.bpd == 1 {
            body.line(read_stmt("series", None, ""));
            if rd.extra.is_none() && dates_with_a_band_name(&first.path) {
                body.line("series = series.squeeze(\"band\", drop=True)  # the bands are dates of one named band");
            }
            if let Some(e) = scaled("series", (first.meta.scale, first.meta.offset)) {
                body.line(format!("series = {e}  # the file's scale and offset"));
            }
        } else if let Some(e) = &info.expr {
            body.line("bands = [");
            for &b in &e.bands {
                let read = if stack_cube {
                    format!("cube.isel(band={})", b - 1)
                } else {
                    format!("zeit.load_raster(src, band={b}, **region)")
                };
                let read = scaled(&read, scale_offset(&first.path, b + first.band_base)).unwrap_or(read);
                body.line(format!("    {read},  # band {b}: {}", name_of(b)));
            }
            body.line("]");
            let (stmts, r) = janus_cube::calc::python::numpy(&e.program, &|k| format!("b[{k}]"), "    ");
            let dates = ["year", "month", "day", "doy", "decyear", "t"]
                .iter()
                .any(|v| format!("{stmts} {r}").split(|c: char| !c.is_alphanumeric() && c != '_').any(|w| w == *v));
            body.line(janus_cube::calc::python::NUMPY_PRELUDE.trim_end());
            body.line(String::new());
            body.line(String::new());
            body.line(format!(
                "def expression(b, year, month, day, doy, decyear, t):\n    \"\"\"{}: the bands read to the value shown.\"\"\"\n\
                 {stmts}    return _out({r})",
                e.text.replace('\n', "; ").replace('"', "'")
            ));
            body.line(String::new());
            body.line(String::new());
            if dates {
                body.line("when = bands[0].indexes[\"time\"]  # the date variables of the expression");
                body.line("doy = when.dayofyear");
                body.line(
                    "dates = [when.year, when.month, when.day, doy, when.year + (doy - 1) / np.where(when.is_leap_year, 366, 365), np.arange(len(when))]",
                );
                body.line("dates = [np.asarray(d, dtype=\"float64\")[:, None, None] for d in dates]");
            }
            body.line(format!(
                "series = bands[0].copy(data=expression([v.values for v in bands], {}))  # the expression: {}",
                if dates { "*dates" } else { "*[0] * 6" },
                e.text.replace('\n', "; ")
            ));
        } else if sel.nd_band > 0 {
            body.line(read_stmt("a", Some(sel.band), &name_of(sel.band)));
            body.line(read_stmt("b", Some(sel.nd_band), &name_of(sel.nd_band)));
            if let Some(e) = scaled("a", (first.meta.scale, first.meta.offset)) {
                body.line(format!("a = {e}  # the files' scale and offset"));
            }
            if let Some(e) = scaled("b", (first.nd_meta.scale, first.nd_meta.offset)) {
                body.line(format!("b = {e}"));
            }
            body.line("series = ((a - b) / (a + b)).where(a + b != 0)  # the normalized difference (a - b) / (a + b)");
        } else {
            body.line(read_stmt("series", Some(sel.band), &name_of(sel.band)));
            if let Some(e) = scaled("series", (first.meta.scale, first.meta.offset)) {
                body.line(format!("series = {e}  # the files' scale and offset"));
            }
        }
        if info.spectral {
            if has("series") {
                body.line("series = series.rename(band=\"time\")  # the spectrum, one value a wavelength");
            }
        } else if !info.time_is_date {
            body.line("series = series.rename(band=\"time\")  # no dates in the files: the bands in order");
        }
    }

    // ---- the quality band
    if qa_on && (want_series || want_bands || want_improphe) {
        body.line(read_stmt("qa", Some(sel.qa_band), &name_of(sel.qa_band)));
        if want_series {
            body.line(format!(
                "series = series.where({})  # the observations the quality band keeps",
                usable(sel.qa_rule)
            ));
        }
    }

    // the cube of some of a date's bands, `list` (a Python list) their names: the stack's cube cut, or a read of each
    let role_cube = |body: &mut Code, var: &str, list: &str, nums: &[i32]| -> Result<(), String> {
        if stack_cube {
            let pos: Vec<i32> = nums.iter().map(|b| b - 1).collect();
            body.line(format!("{var} = cube.isel(band={}).assign_coords(band={list})", ints(&pos)));
        } else if rd.stack {
            return Err("the bands of a date need a file of several bands a date".into());
        } else {
            body.line(format!(
                "{var} = xr.concat([zeit.load_raster(src, band=n, **region) for n in {}], dim=\"band\")",
                ints(nums)
            ));
            body.line(format!("{var} = {var}.assign_coords(band={list}).transpose(\"time\", \"band\", \"y\", \"x\")"));
        }
        // the files' scale and offset
        let so: Vec<(f64, f64)> = nums.iter().map(|&b| scale_offset(&first.path, b + first.band_base)).collect();
        if let Some(l) = scaled_bands(var, &so) {
            body.line(l);
        }
        Ok(())
    };
    let names_list = |names: &[String]| format!("[{}]", names.iter().map(|n| dq(n)).collect::<Vec<_>>().join(", "));

    // ---- the bands by role (CCDC and the tools of surface reflectance)
    let order: Vec<(&str, i32)> = ["blue", "green", "red", "nir", "swir1", "swir2", "thermal"]
        .into_iter()
        .filter_map(|r| spec.roles.get(r).map(|&b| (r, b)))
        .collect();
    if want_bands {
        let missing: Vec<&str> = ["blue", "green", "red", "nir", "swir1", "swir2"]
            .into_iter()
            .filter(|r| !spec.roles.contains_key(*r))
            .collect();
        if !missing.is_empty() && order.iter().all(|(r, _)| *r != "thermal") || order.is_empty() {
            return Err(format!("map the bands by role first: {} missing", missing.join(", ")));
        }
        let names: Vec<&str> = order.iter().map(|(r, _)| *r).collect();
        let nums: Vec<i32> = order.iter().map(|(_, b)| *b).collect();
        body.line(format!("order = {}", strs(&names)));
        role_cube(&mut body, "bands_cube", "order", &nums)?;
    }
    // NBAR: the bands to normalize, and the angle bands unless the angles come from Sentinel-2's metadata
    let mut nbar_names: Vec<&str> = vec![];
    if want_nbar {
        let roles: Vec<(&str, i32)> =
            zeit_core::brdf::ROLES.iter().filter_map(|r| spec.roles.get(*r).map(|&b| (*r, b))).collect();
        if roles.is_empty() {
            return Err("map the bands to normalize by role first".into());
        }
        let nums: Vec<i32> = roles.iter().map(|(_, b)| *b).collect();
        let list = strs(&roles.iter().map(|(r, _)| *r).collect::<Vec<_>>());
        role_cube(&mut body, "nbar_cube", &list, &nums)?;
        nbar_names.push("nbar_cube");
        if calls.iter().any(|(_, _, c)| c.full().contains("nbar_angles")) {
            let angles: Vec<(&str, i32)> =
                ["sza", "saa", "vza", "vaa"].into_iter().filter_map(|r| spec.roles.get(r).map(|&b| (r, b))).collect();
            if angles.len() != 4 {
                return Err("map the four angle bands (sza, saa, vza, vaa) by role first".into());
            }
            let nums: Vec<i32> = angles.iter().map(|(_, b)| *b).collect();
            role_cube(&mut body, "nbar_angles", &strs(&["sza", "saa", "vza", "vaa"]), &nums)?;
            nbar_names.push("nbar_angles");
        }
    }
    // ImproPhe: the fine series (every band of each date but the quality band, the dates it refuses missing) and the
    // coarse one over its whole grid
    if want_improphe {
        let other = spec.other.ok_or("choose the series to sharpen first: ImproPhe's script reads it")?;
        if !info.time_is_date {
            return Err("ImproPhe needs a series with real dates (the seasonal windows are days of the year)".into());
        }
        let band_name = |i: &CubeInfo, b: i32| i.band_names.get(b as usize - 1).cloned().unwrap_or(format!("band {b}"));
        let bands: Vec<i32> = (1..=rd.bpd as i32).filter(|&b| !(qa_on && b == sel.qa_band)).collect();
        let fine_names: Vec<String> = bands.iter().map(|&b| band_name(info, b)).collect();
        body.line("# ImproPhe's fine series: every band of each date but the quality band");
        role_cube(&mut body, "fine_cube", &names_list(&fine_names), &bands)?;
        if qa_on {
            body.line(format!(
                "fine_cube = fine_cube.where({})  # the dates the quality band refuses",
                usable(sel.qa_rule)
            ));
        }
        // the coarse series
        let crd = reads(other, spec.path)?;
        if other.multidim || other.layers.is_empty() {
            return Err("the coarse series is a NetCDF / Zarr variable or has no dates: not in Copy Python yet".into());
        }
        let ofirst = &other.layers[0];
        let cbands: Vec<i32> =
            (1..=crd.bpd as i32).filter(|&b| !(other.sel.qa_band > 0 && b == other.sel.qa_band)).collect();
        let by = calls
            .iter()
            .find(|(_, _, c)| c.inputs.contains(&"improphe_cubes"))
            .map_or(String::new(), |(_, p, _)| p.str("by").to_string());
        let years: Vec<String> = (0..other.t()).map(|t| (other.decimal_year(t).floor() as i64).to_string()).collect();
        let dates = if other.time_is_date { String::new() } else { format!(", dates=[{}]", years.join(", ")) };
        body.line("# the coarse series: every band of each date but its quality band, over its whole grid");
        body.line(format!("coarse_src = {}", crd.src));
        let load = |band: String| format!("zeit.load_raster(coarse_src{band}, masked=True{dates})");
        let cnames: Vec<String> = cbands.iter().map(|&b| band_name(other, b)).collect();
        if crd.stack && crd.bpd > 1 {
            let pos: Vec<i32> = cbands.iter().map(|b| b - 1).collect();
            let cut = if cbands.len() == 1 {
                format!(".isel(band={})", pos[0])
            } else {
                format!(".isel(band={}).assign_coords(band={})", ints(&pos), names_list(&cnames))
            };
            body.line(format!("coarse_cube = {}{cut}", load(String::new())));
        } else if cbands.len() == 1 {
            let band = if crd.bpd > 1 { format!(", band={}", cbands[0]) } else { String::new() };
            body.line(format!("coarse_cube = {}", load(band)));
        } else {
            let read = load(if crd.bpd > 1 { ", band=n".into() } else { String::new() });
            body.line(format!("coarse_cube = xr.concat([{read} for n in {}], dim=\"band\")", ints(&cbands)));
            body.line(format!(
                "coarse_cube = coarse_cube.assign_coords(band={}).transpose(\"time\", \"band\", \"y\", \"x\")",
                names_list(&cnames)
            ));
        }
        let so: Vec<(f64, f64)> = cbands.iter().map(|&b| scale_offset(&ofirst.path, b + ofirst.band_base)).collect();
        if let Some(l) = scaled_bands("coarse_cube", &so) {
            body.line(l);
        }
        if !other.time_is_date {
            if by == "scenes" {
                body.line(format!(
                    "coarse_cube = coarse_cube.assign_coords(time=np.arange({}))  # scenes, not years",
                    other.t()
                ));
            } else {
                body.line(format!(
                    "coarse_cube = coarse_cube.rename(time=\"year\").assign_coords(year=[{}])",
                    years.join(", ")
                ));
            }
        }
    }
    if want_all {
        let nb = rd.bpd;
        let names = format!("[f\"b{{b:03d}}\" for b in range(1, {nb} + 1)]");
        if stack_cube {
            body.line(format!(
                "bands_all = cube.assign_coords(band={names})  # b001, b002...: every band of each date"
            ));
        } else {
            body.line(format!(
                "bands_all = xr.concat([zeit.load_raster(src, band=b, **region) for b in range(1, {nb} + 1)], dim=\"band\")"
            ));
            body.line(format!(
                "bands_all = bands_all.assign_coords(band={names}).transpose(\"time\", \"band\", \"y\", \"x\")"
            ));
        }
        let so: Vec<(f64, f64)> = (1..=nb as i32).map(|b| scale_offset(&first.path, b + first.band_base)).collect();
        if let Some(l) = scaled_bands("bands_all", &so) {
            body.line(l);
        }
    }
    // ---- the pin or the ROI out of the window
    let mut names: Vec<&str> = vec![];
    if want_series {
        names.push("series");
    }
    if want_bands {
        names.push("bands_cube");
        if qa_on {
            names.push("qa");
        }
    }
    names.extend(&nbar_names);
    if want_all {
        names.push("bands_all");
    }
    if let Target::Pin { col, row, .. } = spec.target {
        let (cx, cy) = {
            let g = info.geo_transform.unwrap_or([0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
            (g[0] + (col as f64 + 0.5) * g[1], g[3] + (row as f64 + 0.5) * g[5])
        };
        if names.len() > 1 {
            body.line(format!("pixel = dict(x=[{}], y=[{}], method=\"nearest\")", num(cx), num(cy)));
            for n in &names {
                body.line(format!("{n} = {n}.sel(**pixel).compute()"));
            }
        } else {
            body.line(format!(
                "{0} = {0}.sel(x=[{1}], y=[{2}], method=\"nearest\").compute()",
                names[0],
                num(cx),
                num(cy)
            ));
        }
    } else if roi {
        body.line("series = series.mean([\"y\", \"x\"], keepdims=True).compute()  # the mean of the cells inside, at each date");
    }
    if want_all {
        body.line("bands_all = bands_all.where(series.notnull())  # left out where the shown series is");
    }
    if want_spectrum {
        let names = "[f\"b{i + 1:03d}\" for i in range(series.sizes[\"band\"])]";
        body.line(format!("spectrum = series.assign_coords(band={names})  # b001, b002...: one value a wavelength"));
    }

    // ---- the reflectance for the tools of bands
    if want_bands {
        let units = calls
            .iter()
            .find(|(_, _, c)| c.inputs.contains(&"bands_cube"))
            .map(|(_, p, _)| p.str("units").to_string())
            .unwrap_or_default();
        let optical = order.iter().any(|(r, _)| *r == "thermal");
        let target = if optical { "bands_cube.sel(band=optical)" } else { "bands_cube" };
        if optical {
            body.line("optical = [b for b in order if b != \"thermal\"]  # the thermal band is left as it is");
        }
        let (conv_unit, conv_c2) = (format!("{target} * 10000"), format!("({target} * 2.75e-05 - 0.2) * 10000"));
        let assign_to = |e: &str| {
            if optical { format!("bands_cube.loc[dict(band=optical)] = {e}") } else { format!("bands_cube = {e}") }
        };
        match units.as_str() {
            "x10000" => {}
            "unit" => {
                body.line("# Zeit takes reflectance x 10000; these are 0-1");
                body.line(assign_to(&conv_unit));
            }
            "landsat_c2" => {
                body.line("# Zeit takes reflectance x 10000; these are Landsat Collection 2 digital numbers");
                body.line(assign_to(&conv_c2));
            }
            _ => {
                body.line("# Zeit takes reflectance x 10000: Janus tells the units by the median (<= 2: 0-1; >= 6500: Landsat Collection 2 digital numbers)");
                body.line(format!("median = float({target}.median())"));
                body.line(assign_to(&format!(
                    "{conv_unit} if median <= 2 else {conv_c2} if median >= 6500 else {target}"
                )));
            }
        }
        // the quality band as Fmask codes
        let banded: Vec<&ToolCall> = calls.iter().map(|c| &c.2).filter(|c| c.inputs.contains(&"bands_cube")).collect();
        // CCDC gets a date missing a band as Janus gives it: zeros, out of the valid range, flagged as a cloud
        let ccdc = banded.iter().any(|c| matches!(c.tool.as_str(), "ccdc" | "ccdc_synthetic"));
        body.line("complete = bands_cube.notnull().all(\"band\")  # a date missing a band is no observation");
        if qa_on {
            body.line("c = qa.fillna(0).astype(\"int64\")");
            body.line(format!(
                "fmask = {}  # {} as Fmask codes: 0 clear, 1 water, 2 shadow, 3 snow, 4 cloud, 255 none",
                fmask_of(sel.qa_rule),
                sel.qa_rule.label()
            ));
            body.line(if ccdc {
                "fmask = fmask.where(qa >= 0, 255).where(complete, 4)  # no code: 255; a band missing: a cloud"
            } else {
                "fmask = fmask.where((qa >= 0) & complete, 255)"
            });
        } else if ccdc {
            body.line("fmask = xr.where(complete, 0, 4)  # no quality band: every complete observation is clear");
        } else {
            body.line("fmask = xr.where(complete, 0, 255)  # no quality band: every complete observation is clear");
        }
        if ccdc {
            body.line("bands_cube = bands_cube.fillna(0)  # CCDC reads a missing value as one out of its valid range");
        }
        // NDFI, CODED and Tmask work on the clear observations (CCDC and S-CCD take the codes themselves)
        if banded.iter().all(|c| !c.inputs.contains(&"fmask")) {
            body.line("bands_cube = bands_cube.where(fmask <= 1)  # only the clear observations (Fmask 0 and 1)");
        } else if banded.iter().any(|c| !c.inputs.contains(&"fmask")) {
            body.line("# NDFI, CODED and Tmask use only the clear observations: bands_cube.where(fmask <= 1)");
        }
    }

    // ---- the tools
    let many = calls.len() > 1;
    let mut printed: Vec<String> = vec![];
    // (the result's variable, the file's stem) of every map a run writes
    let mut saves: Vec<(String, String)> = vec![];
    let mut tool_code = Code::default();
    let mut used: Vec<&str> = vec![];
    for (k, item) in sequence.iter().enumerate() {
        let i = match item {
            Ok(i) => *i,
            Err(note) => {
                tool_code.blank();
                tool_code.line(note.clone());
                continue;
            }
        };
        let (name, _, call) = &calls[i];
        let step = spec.chain.get(k);
        // the variables of a chain's step are named by its tool, and by its place if the tool comes again
        let repeats = used.iter().filter(|t| **t == call.tool).count();
        used.push(call.tool.as_str());
        let prefix = match (step, many) {
            (Some(_), _) if repeats > 0 => format!("{}{}_", call.tool, repeats + 1),
            (Some(_), _) | (None, true) => format!("{}_", call.tool),
            (None, false) => String::new(),
        };
        let (code, results) = call.compact(&prefix);
        tool_code.blank();
        if let Some(s) = step {
            let reads = if s.reads_analysis { "the analysis series" } else { "the series" };
            tool_code.line(format!("# {}. {name}, on {reads}", k + 1));
        } else if many {
            tool_code.line(format!("# {name}"));
        }
        if call.needs_values {
            tool_code.line(format!(
                "values = {}.values.ravel()",
                if step.is_some_and(|s| s.reads_analysis) { "analysis" } else { "series" }
            ));
        }
        tool_code.line(code);
        if let (Some(s), Some(first)) = (step, results.first())
            && s.leaves_analysis
        {
            tool_code.line(format!("analysis = {first}  # the analysis series: the steps below read it"));
        }
        for r in &results {
            let stem = match step {
                Some(s) if results.len() > 1 => format!("{}_{}", s.name, r.strip_prefix(&prefix).unwrap_or(r)),
                Some(s) => s.name.clone(),
                None => r.strip_prefix(&prefix).unwrap_or(r).to_string(),
            };
            saves.push((r.clone(), stem));
        }
        printed.extend(results);
    }
    if calls.is_empty() {
        body.blank();
        for line in &left_out {
            body.line(line.clone());
        }
        if !run {
            body.line("print(series.squeeze().to_series())  # the series as Janus shows it");
        }
    } else {
        for line in &left_out {
            tool_code.blank();
            tool_code.line(line.clone());
        }
        body.0.extend(tool_code.0);
        body.blank();
        if !run {
            for r in &printed {
                body.line(format!("print({r})"));
            }
        }
    }
    // a run writes its maps: a Dataset (a result with several maps) as a folder of GeoTIFFs, a cube as one file
    if let Target::Run { out_dir, .. } = &spec.target {
        if saves.is_empty() {
            return Err("no step can run".into());
        }
        body.line(format!("out_dir = {}", dq(out_dir)));
        for (var, stem) in &saves {
            body.line(format!("zeit.save_raster({var}, f\"{{out_dir}}/{stem}\")"));
        }
    }

    // ---- the whole script
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (y, m, d) = crate::numeric::civil(now.div_euclid(86400));
    let what = match &spec.target {
        Target::Pin { id, .. } => format!("pin {id}"),
        Target::Roi(r) if r.rings.is_empty() => "the ROI (a rectangle)".to_string(),
        Target::Roi(_) => "the ROI (a polygon)".to_string(),
        Target::Run { what, .. } => what.clone(),
    };
    // a tool's own `import pandas as pd` is the header's
    let text = body.text().replace("import pandas as pd\n", "");
    c.line(format!(
        "\"\"\"Janus {} ({y:04}-{m:02}-{d:02}): {what} {} {}, reproduced outside Janus with Zeit.\nNeeds: pip install zeit-cdts\n\"\"\"",
        spec.version,
        if run { "on" } else { "of" },
        spec.name
    ));
    if text.contains("np.") {
        c.line("import numpy as np");
    }
    if text.contains("pd.") {
        c.line("import pandas as pd");
    }
    if text.contains("xr.") {
        c.line("import xarray as xr");
    }
    c.line("import zeit");
    c.blank();
    let mut out = c.text();
    out += "\n";
    out += &text;
    out += "\n";
    Ok(out)
}
