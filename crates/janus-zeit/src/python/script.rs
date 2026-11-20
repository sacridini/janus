//! "Copy Python" of a pin or an ROI: the script that reads the series Janus shows with Zeit itself (`load_raster`:
//! the dates, the bands, the window; the few lines of xarray for what Zeit has no function for: a normalized
//! difference, a quality band's rule, the units of the reflectance) and makes the same Zeit calls with the same
//! parameters. The app's chart and the website's guides both call `pixel_script`.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::fmt::Write as _;

use janus_cube::{CubeInfo, QaRule};

use super::{ToolCall, tool_call, wrap_call};
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
    /// an Analysis-ready data chain's step reads the bands of the harmonized series (what an earlier NBAR or spectral
    /// adjustment left), not the files'
    pub reads_harmonized: bool,
    /// the step's first result makes the harmonized series: the Python expression of it, `{}` standing for the result
    pub harmonizes: Option<String>,
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

/// A quality rule as `load_raster(qa_rule=)` and `zeit.qa_mask` name it.
fn rule_arg(rule: QaRule) -> &'static str {
    match rule {
        QaRule::Fmask | QaRule::None => "fmask",
        QaRule::LandsatC2 => "landsat_c2",
        QaRule::Scl => "scl",
        QaRule::HlsFmask => "hls_fmask",
        QaRule::NonZero => "nonzero",
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
    info.require_float_bands("Copy Python")?;
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
    /// the pin's `pixel=(col, row)`: Zeit reads that pixel only
    pixel: Option<(i64, i64)>,
    /// `window=(col0, row0, col1, row1)`, the end exclusive, of a rectangle that is not the whole raster
    cells: Option<[usize; 4]>,
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
    // Zeit reads the raster's own pixels (`pixel=`, `window=`), with no CRS needed, but a NetCDF / Zarr variable's
    // rows may run either way: its windows are cut by coordinates
    let by_pixels = !info.multidim;
    Ok(match &spec.target {
        Target::Run { window: [x0, y0, x1, y1], .. } => {
            let (w, h) = (x1 - x0, y1 - y0);
            let whole = (*x0, *y0, *x1, *y1) == (0, 0, info.width, info.height);
            let note = if whole {
                format!("The run's window: the whole raster ({w} x {h} pixels)")
            } else {
                format!("The run's window: x {x0}..{x1}, y {y0}..{y1} of the files' pixels ({w} x {h})")
            };
            let cells = (by_pixels && !whole).then_some([*x0, *y0, *x1, *y1]);
            let (x0, y0, x1, y1) = (*x0 as f64, *y0 as f64, *x1 as f64, *y1 as f64);
            let clip = (!by_pixels && !whole).then(|| bounds(x0 + 0.4, y0 + 0.4, x1 - 0.4, y1 - 0.4));
            Window { clip, polygon: None, note, pixel: None, cells }
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
            Window { clip: None, polygon: None, note, pixel: by_pixels.then_some((*col, *row)), cells: None }
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
                let whole = (roi.x0, roi.y0, roi.x1, roi.y1) == (0, 0, info.width, info.height);
                let cells = (by_pixels && !whole).then_some([roi.x0, roi.y0, roi.x1, roi.y1]);
                let (x0, y0, x1, y1) = (roi.x0 as f64, roi.y0 as f64, roi.x1 as f64, roi.y1 as f64);
                let clip = (!by_pixels).then(|| bounds(x0 + 0.4, y0 + 0.4, x1 - 0.4, y1 - 0.4));
                Window { clip, polygon: None, note, pixel: None, cells }
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
                    pixel: None,
                    cells: None,
                }
            }
        }
    })
}

/// Whether a file is named for a month (`chirps_2020-01.tif`: four digits, a dash, two digits, and no day after them).
fn month_name(path: &str) -> bool {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let stem = name.rsplit_once('.').map_or(name, |s| s.0).as_bytes();
    let digits = |at: usize, n: usize| stem.get(at..at + n).is_some_and(|d| d.iter().all(u8::is_ascii_digit));
    (0..stem.len()).any(|i| {
        let boundary = i == 0 || !stem[i - 1].is_ascii_digit();
        let day = stem.get(i + 7) == Some(&b'-') && digits(i + 8, 2);
        boundary && digits(i, 4) && stem.get(i + 4) == Some(&b'-') && digits(i + 5, 2) && !digits(i + 7, 1) && !day
    })
}

/// A Python list of ints.
fn ints(v: &[i32]) -> String {
    format!("[{}]", v.iter().map(i32::to_string).collect::<Vec<_>>().join(", "))
}

/// What `pip install` needs for a script of Zeit's `eof` or `lagged_correlation` (the Zeit that has them: to be confirmed
/// when it is released).
const ZEIT_WITH_EOF: &str = "\"zeit-cdts>=3.8\"";

/// What `pip install` needs for a script of Zeit's `mnf`, `cva`, `imad`, `radcal`, `omnibus` or `topographic_correction` (Zeit 3.9).
const ZEIT_WITH_CHANGE: &str = "\"zeit-cdts>=3.9\"";

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
        band_numbers: spec.roles.clone(),
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
        if id == "topocorr" {
            input.suns = crate::sun::of_series(info);
            input.bands_per_date = info.bands_per_date;
            input.qa_band = info.sel.qa_band;
            let window = match &spec.target {
                Target::Run { window, .. } => *window,
                _ => [0, 0, info.width, info.height],
            };
            input.sun_centre = info.geo_transform.and_then(|g| crate::sun::centre_lat_lon(&info.crs_wkt, &g, window));
        }
        // a tool the Zeit calls cannot be written for (it needs dates the series lacks, or endmembers) is left out
        let note = match tool_call(id, params, &input) {
            // the bands of a date are one pixel's: an ROI has the mean of its series only
            Some(c) if roi && c.inputs != ["series"] => {
                format!("# {}: its bands are a pin's, not the ROI's mean", tool.name)
            }
            // the two series ImproPhe reads, and the driver series of a lagged correlation, are a run's
            Some(c) if (c.inputs.contains(&"improphe_cubes") || c.inputs.contains(&"other_cube")) && !run => {
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
    // another series the call reads (a lagged correlation's driver), over its whole grid
    let want_other = has("other_cube");
    // whether each call reads the harmonized series (an Analysis-ready data chain's step after NBAR or the adjustment)
    let mut harm_call = vec![false; calls.len()];
    for (k, item) in sequence.iter().enumerate() {
        if let Ok(i) = item {
            harm_call[*i] = spec.chain.get(k).is_some_and(|s| s.reads_harmonized);
        }
    }
    let reads_files = |input: &str| calls.iter().zip(&harm_call).any(|((_, _, c), h)| c.inputs.contains(&input) && !*h);
    let want_bands_files = reads_files("bands_cube");
    let want_improphe_files = reads_files("improphe_cubes");
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

    let stack_cube = rd.stack && rd.bpd > 1;
    // Zeit's NBAR wants the cube's y and x, and the post-processing works on maps: the pixel is a window of one
    // cell then, not a series
    let want_grid = want_nbar || calls.iter().any(|(_, _, c)| c.tool == "postproc");
    let pixel_mode = win.pixel.is_some() && !want_grid;
    // what every read is told about the region: a pixel or a window of the files' pixels is all Zeit reads; a polygon
    // (or a NetCDF window) is read by blocks, then cut
    let mut region: Vec<String> = vec![];
    if let Some((col, row)) = win.pixel {
        region.push(if pixel_mode {
            format!("pixel=({col}, {row})")
        } else {
            format!("window=({col}, {row}, {}, {})", col + 1, row + 1)
        });
    } else if let Some([x0, y0, x1, y1]) = win.cells {
        region.push(format!("window=({x0}, {y0}, {x1}, {y1})"));
    } else {
        if !run {
            region.push(format!("chunks={CHUNKS}"));
        }
        region.extend(win.clip.as_ref().map(|c| format!("clip={c}")));
    }
    let lazy = region.iter().any(|r| r.starts_with("chunks="));
    // whether the script needs a Zeit that has what `load_raster` gained (scale=, qa=, bands=, window=, pixel=)
    let new_zeit = Cell::new(win.pixel.is_some() || win.cells.is_some());
    let loads = Cell::new(0usize);
    // whether it needs Zeit 3.2.1: `qa_to_fmask`, NaN in CCDC's and S-CCD's bands, a pixel for CODED and Tmask
    let zeit_321 = Cell::new(false);

    // whether a band of the files has a scale or an offset (`load_raster(scale=True)` applies them)
    let needs_scale = |nums: &[i32]| -> bool {
        if rd.bpd == 1 {
            return (first.meta.scale, first.meta.offset) != (1.0, 0.0);
        }
        nums.iter().any(|&b| scale_offset(&first.path, b + first.band_base) != (1.0, 0.0))
    };
    let name_of =
        |n: i32| -> String { info.band_names.get(n as usize - 1).cloned().unwrap_or_else(|| format!("band {n}")) };
    let names_list = |names: &[String]| format!("[{}]", names.iter().map(|n| dq(n)).collect::<Vec<_>>().join(", "));
    // the names of a date's bands as the cube's `band` coordinate: each one once
    let unique = |names: Vec<String>| -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        names
            .into_iter()
            .enumerate()
            .map(|(k, n)| if seen.insert(n.clone()) { n } else { format!("{n}_{}", k + 1) })
            .collect()
    };

    // ---- the bands by role (CCDC and the tools of surface reflectance)
    let order: Vec<(&str, i32)> = ["blue", "green", "red", "nir", "swir1", "swir2", "thermal"]
        .into_iter()
        .filter_map(|r| spec.roles.get(r).map(|&b| (r, b)))
        .collect();
    // the bands' names as a variable, when the tools' code refers to it
    let order_used = order.iter().any(|(r, _)| *r == "thermal")
        || calls.iter().any(|(_, _, c)| c.inputs.contains(&"bands_cube") && c.full().contains("order"));

    // the reads of the series and of the bands; the first pass counts them, the second names the files once if
    // there are several
    let read_part = |use_var: bool, shared: bool| -> Result<Code, String> {
        loads.set(0);
        let mut body = Code::default();
        // the arguments of a read: `sel_args` the bands, `scale` the files' scale and offset, `qa` the quality band
        // that masks what is read
        let args = |sel_args: Vec<String>, scale: bool, qa: Option<(i32, QaRule)>| -> Vec<String> {
            loads.set(loads.get() + 1);
            let mut a = vec![if use_var { "src".to_string() } else { rd.src.clone() }];
            a.extend(rd.extra.clone());
            a.extend(sel_args);
            if scale {
                new_zeit.set(true);
                a.push("scale=True".into());
            }
            if let Some((band, rule)) = qa {
                new_zeit.set(true);
                a.push(format!("qa={band}"));
                a.push(format!("qa_rule=\"{}\"", rule_arg(rule)));
            }
            if shared {
                a.push("**region".into());
            } else {
                a.push("masked=True".into());
                a.extend(region.iter().cloned());
            }
            a
        };
        let stmt = |var: &str, a: Vec<String>, comment: &str| -> String {
            let text = wrap_call(var, "zeit.load_raster", &a);
            if comment.is_empty() { text } else { format!("{text}  # {comment}") }
        };
        // `band=n` in a file of several bands a date
        let band_arg = |n: i32| -> Vec<String> { if rd.bpd > 1 { vec![format!("band={n}")] } else { vec![] } };
        let qa_of = || qa_on.then_some((sel.qa_band, sel.qa_rule));
        let role_cube = |body: &mut Code,
                         var: &str,
                         names: &[String],
                         nums: &[i32],
                         list: &str|
         -> Result<(), String> {
            if stack_cube {
                let pos: Vec<i32> = nums.iter().map(|b| b - 1).collect();
                body.line(format!("{var} = cube.isel(band={}).assign_coords(band={list})", ints(&pos)));
            } else if rd.stack {
                return Err("the bands of a date need a file of several bands a date".into());
            } else {
                new_zeit.set(true);
                let dict: Vec<String> = names.iter().zip(nums).map(|(n, b)| format!("{}: {b}", dq(n))).collect();
                body.line(stmt(var, args(vec![format!("bands={{{}}}", dict.join(", "))], needs_scale(nums), None), ""));
            }
            Ok(())
        };

        body.line(format!("# {}", win.note));
        if let Some(p) = &win.polygon {
            body.line(p);
        }
        if use_var {
            body.line(format!("src = {}", rd.src));
        }
        if shared {
            body.line(format!("region = dict(masked=True, {})", region.join(", ")));
        }
        if stack_cube {
            let all: Vec<i32> = (1..=rd.bpd as i32).collect();
            let what = if pixel_mode {
                "(time, band): the bands of each date"
            } else {
                "(time, band, y, x): the bands of each date"
            };
            body.line(stmt("cube", args(vec![], needs_scale(&all), None), what));
        }

        // ---- the series Janus shows
        let mut qa_folded = false;
        if want_series {
            if rd.stack && rd.bpd == 1 {
                if let (Some(crs), Some(clip)) = (&rd.assumed_crs, &win.clip) {
                    // the file has no CRS: say it, then cut the window
                    loads.set(loads.get() + 1);
                    let cut = if win.polygon.is_some() {
                        "clip([polygon], drop=True)".to_string()
                    } else {
                        format!("clip_box({})", clip.trim_start_matches('(').trim_end_matches(')'))
                    };
                    let extra = rd.extra.as_ref().map_or(String::new(), |e| format!(", {e}"));
                    body.line(format!(
                        "series = zeit.load_raster({}{extra}, masked=True)\nseries = series.rio.write_crs(\"{crs}\").rio.{cut}",
                        if use_var { "src" } else { rd.src.as_str() }
                    ));
                } else {
                    body.line(stmt("series", args(vec![], needs_scale(&[1]), None), ""));
                }
                if rd.extra.is_none() && dates_with_a_band_name(&first.path) {
                    body.line("series = series.squeeze(\"band\", drop=True)  # the bands are dates of one named band");
                }
            } else if let Some(e) = &info.expr {
                body.line("bands = [");
                for &b in &e.bands {
                    let read = if stack_cube {
                        format!("cube.isel(band={})", b - 1)
                    } else {
                        let scale = needs_scale(&[b]);
                        format!("zeit.load_raster({})", args(band_arg(b), scale, None).join(", "))
                    };
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
                    // one value a date for a pixel, a (time, y, x) cube otherwise
                    let pad = if pixel_mode { "" } else { "[:, None, None]" };
                    body.line("when = bands[0].indexes[\"time\"]  # the date variables of the expression");
                    body.line("doy = when.dayofyear");
                    body.line(
                        "dates = [when.year, when.month, when.day, doy, when.year + (doy - 1) / np.where(when.is_leap_year, 366, 365), np.arange(len(when))]",
                    );
                    body.line(format!("dates = [np.asarray(d, dtype=\"float64\"){pad} for d in dates]"));
                }
                body.line(format!(
                    "series = bands[0].copy(data=expression([v.values for v in bands], {}))  # the expression: {}",
                    if dates { "*dates" } else { "*[0] * 6" },
                    e.text.replace('\n', "; ")
                ));
            } else if sel.nd_band > 0 {
                if stack_cube {
                    new_zeit.set(true);
                    body.line(format!(
                        "series = zeit.nd(cube.isel(band={}), cube.isel(band={}))  # {} and {}: (a - b) / (a + b)",
                        sel.band - 1,
                        sel.nd_band - 1,
                        name_of(sel.band),
                        name_of(sel.nd_band)
                    ));
                } else {
                    new_zeit.set(true);
                    let label = |n: i32, fallback: &str| -> String {
                        spec.roles.iter().find(|(_, b)| **b == n).map_or(fallback.to_string(), |(r, _)| r.clone())
                    };
                    let (a, b) = (label(sel.band, "a"), label(sel.nd_band, "b"));
                    let dict = format!("bands={{{}: {}, {}: {}}}", dq(&a), sel.band, dq(&b), sel.nd_band);
                    body.line(stmt(
                        "cube",
                        args(vec![dict], needs_scale(&[sel.band, sel.nd_band]), qa_of()),
                        &format!("{} and {}", name_of(sel.band), name_of(sel.nd_band)),
                    ));
                    body.line(format!("series = zeit.normalized_difference(cube, {}, {})", dq(&a), dq(&b)));
                    qa_folded = qa_on;
                }
            } else if stack_cube {
                body.line(format!("series = cube.isel(band={})  # {}", sel.band - 1, name_of(sel.band)));
            } else {
                qa_folded = qa_on;
                body.line(stmt(
                    "series",
                    args(band_arg(sel.band), needs_scale(&[sel.band]), qa_of()),
                    &if rd.bpd > 1 { name_of(sel.band) } else { String::new() },
                ));
            }
            if info.spectral {
                if has("series") {
                    body.line("series = series.rename(band=\"time\")  # the spectrum, one value a wavelength");
                }
            } else if !info.time_is_date {
                body.line("series = series.rename(band=\"time\")  # no dates in the files: the bands in order");
            }
        }

        // ---- the quality band: the series masked by it, or its codes for the tools of bands
        if qa_on && (want_series || want_bands || want_improphe) {
            let codes = want_bands || want_improphe;
            let masks = want_series && !qa_folded;
            if codes || masks {
                let q = sel.qa_band;
                let qa_expr =
                    if stack_cube && !codes { format!("cube.isel(band={})", q - 1) } else { "qa".to_string() };
                if codes && stack_cube {
                    body.line(format!("qa = cube.isel(band={})  # {}", q - 1, name_of(q)));
                } else if !stack_cube {
                    body.line(stmt("qa", args(band_arg(q), false, None), &name_of(q)));
                }
                if masks {
                    new_zeit.set(true);
                    body.line(format!(
                        "series = series.where(zeit.qa_mask({qa_expr}, \"{}\"))  # the observations the quality band keeps",
                        rule_arg(sel.qa_rule)
                    ));
                }
            }
        }

        let mut names: Vec<&str> = vec![];
        if want_series {
            names.push("series");
        }
        if want_bands_files {
            let missing: Vec<&str> = ["blue", "green", "red", "nir", "swir1", "swir2"]
                .into_iter()
                .filter(|r| !spec.roles.contains_key(*r))
                .collect();
            if !missing.is_empty() && order.iter().all(|(r, _)| *r != "thermal") || order.is_empty() {
                return Err(format!("map the bands by role first: {} missing", missing.join(", ")));
            }
            let names_v: Vec<String> = order.iter().map(|(r, _)| r.to_string()).collect();
            let nums: Vec<i32> = order.iter().map(|(_, b)| *b).collect();
            let list = if order_used {
                body.line(format!("order = {}", names_list(&names_v)));
                "order".to_string()
            } else {
                names_list(&names_v)
            };
            role_cube(&mut body, "bands_cube", &names_v, &nums, &list)?;
            names.push("bands_cube");
            if qa_on {
                names.push("qa");
            }
        }
        // NBAR: the bands to normalize, and the angle bands unless the angles come from Sentinel-2's metadata
        if want_nbar {
            let roles: Vec<(&str, i32)> =
                zeit_core::brdf::ROLES.iter().filter_map(|r| spec.roles.get(*r).map(|&b| (*r, b))).collect();
            if roles.is_empty() {
                return Err("map the bands to normalize by role first".into());
            }
            let names_v: Vec<String> = roles.iter().map(|(r, _)| r.to_string()).collect();
            let nums: Vec<i32> = roles.iter().map(|(_, b)| *b).collect();
            role_cube(&mut body, "nbar_cube", &names_v, &nums, &names_list(&names_v))?;
            names.push("nbar_cube");
            if calls.iter().any(|(_, _, c)| c.full().contains("nbar_angles")) {
                let angles: Vec<(&str, i32)> = ["sza", "saa", "vza", "vaa"]
                    .into_iter()
                    .filter_map(|r| spec.roles.get(r).map(|&b| (r, b)))
                    .collect();
                if angles.len() != 4 {
                    return Err("map the four angle bands (sza, saa, vza, vaa) by role first".into());
                }
                let names_v: Vec<String> = angles.iter().map(|(r, _)| r.to_string()).collect();
                let nums: Vec<i32> = angles.iter().map(|(_, b)| *b).collect();
                role_cube(&mut body, "nbar_angles", &names_v, &nums, &names_list(&names_v))?;
                names.push("nbar_angles");
            }
        }
        // ImproPhe: the fine series (every band of each date but the quality band, the dates it refuses missing) and the
        // coarse one over its whole grid
        if want_improphe || want_other {
            // the other series' variable: ImproPhe's coarse series, or a call's `other_cube`
            let cvar = if want_improphe { "coarse_cube" } else { "other_cube" };
            let other = spec.other.ok_or(if want_improphe {
                "choose the series to sharpen first: ImproPhe's script reads it"
            } else {
                "choose the driver series first: the script reads it"
            })?;
            if !info.time_is_date {
                return Err(if want_improphe {
                    "ImproPhe needs a series with real dates (the seasonal windows are days of the year)"
                } else {
                    "a lagged correlation needs a series with real dates"
                }
                .into());
            }
            let band_name =
                |i: &CubeInfo, b: i32| i.band_names.get(b as usize - 1).cloned().unwrap_or(format!("band {b}"));
            if want_improphe && want_improphe_files {
                let bands: Vec<i32> = (1..=rd.bpd as i32).filter(|&b| !(qa_on && b == sel.qa_band)).collect();
                let fine_names = unique(bands.iter().map(|&b| band_name(info, b)).collect());
                body.line("# ImproPhe's fine series: every band of each date but the quality band");
                role_cube(&mut body, "fine_cube", &fine_names, &bands, &names_list(&fine_names))?;
                if qa_on {
                    new_zeit.set(true);
                    body.line(format!(
                        "fine_cube = fine_cube.where(zeit.qa_mask(qa, \"{}\"))  # the dates the quality band refuses",
                        rule_arg(sel.qa_rule)
                    ));
                }
            }
            // the coarse series
            let crd = reads(other, spec.path)?;
            if other.multidim || other.layers.is_empty() {
                return Err(
                    "the coarse series is a NetCDF / Zarr variable or has no dates: not in Copy Python yet".into()
                );
            }
            let ofirst = &other.layers[0];
            let cbands: Vec<i32> =
                (1..=crd.bpd as i32).filter(|&b| !(other.sel.qa_band > 0 && b == other.sel.qa_band)).collect();
            // a driver is one series: its first band of each date
            let cbands: Vec<i32> = if want_improphe { cbands } else { cbands.into_iter().take(1).collect() };
            if want_other && !want_improphe && !other.time_is_date {
                return Err("the driver series needs real dates: not in Copy Python for one without".into());
            }
            let by = calls
                .iter()
                .find(|(_, _, c)| c.inputs.contains(&"improphe_cubes"))
                .map_or(String::new(), |(_, p, _)| p.str("by").to_string());
            let years: Vec<String> =
                (0..other.t()).map(|t| (other.decimal_year(t).floor() as i64).to_string()).collect();
            // (Zeit reads a file named for a month, `chirps_2020-01.tif`, as the year's first day: the format says so)
            let dates = if !other.time_is_date {
                format!(", dates=[{}]", years.join(", "))
            } else if other.layers.len() > 1 && other.layers.iter().all(|l| month_name(&l.path)) {
                ", date_format=\"%Y-%m\"".to_string()
            } else {
                String::new()
            };
            let scale = if crd.bpd == 1 {
                (ofirst.meta.scale, ofirst.meta.offset) != (1.0, 0.0)
            } else {
                cbands.iter().any(|&b| scale_offset(&ofirst.path, b + ofirst.band_base) != (1.0, 0.0))
            };
            let scale_arg = if scale { ", scale=True" } else { "" };
            // a lagged correlation's driver is put on the series' grid, as the tool does (Zeit's call wants it there)
            let like = if want_other && !want_improphe {
                let how =
                    calls.iter().find(|(_, _, c)| c.tool == "lagged_correlation").map_or("nearest", |(_, p, _)| {
                        if p.str("resampling") == "bilinear" { "bilinear" } else { "nearest" }
                    });
                format!(", like=series, resampling='{how}'")
            } else {
                String::new()
            };
            new_zeit.set(new_zeit.get() || scale);
            body.line(if want_improphe {
                "# the coarse series: every band of each date but its quality band, over its whole grid"
            } else {
                "# the driver series: its first band of each date, over its whole grid, put on the series' grid"
            });
            body.line(format!("coarse_src = {}", crd.src));
            let load =
                |extra: String| format!("zeit.load_raster(coarse_src{extra}{scale_arg}, masked=True{dates}{like})");
            let cnames = unique(cbands.iter().map(|&b| band_name(other, b)).collect());
            if crd.stack && crd.bpd > 1 {
                let pos: Vec<i32> = cbands.iter().map(|b| b - 1).collect();
                let cut = if cbands.len() == 1 {
                    format!(".isel(band={})", pos[0])
                } else {
                    format!(".isel(band={}).assign_coords(band={})", ints(&pos), names_list(&cnames))
                };
                body.line(format!("{cvar} = {}{cut}", load(String::new())));
            } else if cbands.len() == 1 {
                let band = if crd.bpd > 1 { format!(", band={}", cbands[0]) } else { String::new() };
                body.line(format!("{cvar} = {}", load(band)));
            } else {
                new_zeit.set(true);
                let dict: Vec<String> = cnames.iter().zip(&cbands).map(|(n, b)| format!("{}: {b}", dq(n))).collect();
                body.line(format!("{cvar} = {}", load(format!(", bands={{{}}}", dict.join(", ")))));
            }
            if want_improphe && !other.time_is_date {
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
            new_zeit.set(true);
            let nb = rd.bpd;
            if stack_cube {
                let labels = format!("[f\"b{{b:03d}}\" for b in range(1, {nb} + 1)]");
                body.line(format!(
                    "bands_all = cube.assign_coords(band={labels})  # b001, b002...: every band of each date"
                ));
            } else {
                let all: Vec<i32> = (1..=nb as i32).collect();
                let dict = format!("bands={{f\"b{{b:03d}}\": b for b in range(1, {nb} + 1)}}");
                body.line(stmt(
                    "bands_all",
                    args(vec![dict], needs_scale(&all), None),
                    "b001, b002...: every band of each date",
                ));
            }
            names.push("bands_all");
        }
        // ---- the pin or the ROI out of the window
        if let Target::Pin { col, row, .. } = spec.target
            && win.pixel.is_none()
        {
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
            body.line(format!(
                "series = series.mean([\"y\", \"x\"]){}  # the mean of the cells inside, at each date",
                if lazy { ".compute()" } else { "" }
            ));
        }
        if want_all {
            body.line("bands_all = bands_all.where(series.notnull())  # left out where the shown series is");
        }
        if want_spectrum {
            let names = "[f\"b{i + 1:03d}\" for i in range(series.sizes[\"band\"])]";
            body.line(format!(
                "spectrum = series.assign_coords(band={names})  # b001, b002...: one value a wavelength"
            ));
        }
        Ok(body)
    };
    let mut body = read_part(false, false)?;
    if loads.get() > 1 || rd.src.starts_with('[') {
        body = read_part(true, loads.get() > 1 && win.polygon.is_some())?;
    }
    let mut c = Code::default();

    // ---- the tools of bands: the reflectance units, the clear observations
    // what each banded call is changed to: the quality codes and the units. The calls that read the files share one
    // preparation of `bands_cube`; a call that reads the harmonized series has its own, after the steps before it
    #[derive(Clone, Default)]
    struct Prep {
        qa: Option<Option<String>>,
        scale: Option<String>,
    }
    let prep = |idx: &[usize], harmonized: bool, out: &mut Code| -> Prep {
        let mut prep = Prep::default();
        let units = calls[idx[0]].1.str("units").to_string();
        let optical = !harmonized && order.iter().any(|(r, _)| *r == "thermal");
        let target = if optical { "bands_cube.sel(band=optical)" } else { "bands_cube" };
        if optical {
            out.line("optical = [b for b in order if b != \"thermal\"]  # the thermal band is left as it is");
        }
        let banded: Vec<&ToolCall> = idx.iter().map(|&i| &calls[i].2).collect();
        let is_cd = |t: &str| matches!(t, "ccdc" | "ccdc_synthetic" | "sccd");
        // CCDC and S-CCD read a date missing a band as no observation, and take the quality band's Fmask codes
        let only_cd = banded.iter().all(|c| is_cd(&c.tool));
        if banded.iter().any(|c| is_cd(&c.tool)) {
            zeit_321.set(true);
        }
        // Zeit's own units: `scale=` of the tools that have it (not Tmask, Tasseled Cap, nor the thermal band of the
        // tools that do not leave it as it is)
        let native = banded
            .iter()
            .all(|c| is_cd(&c.tool) || (!optical && matches!(c.tool.as_str(), "ndfi" | "coded" | "spectral_adjust")));
        // `scale="auto"` tells 0-1 from x10000 but not Landsat Collection 2's digital numbers (>= 6500), which a
        // series may be unless its files carry the scale and offset or its sensors are known not to be Landsat
        let optical_nums: Vec<i32> = order.iter().filter(|(r, _)| *r != "thermal").map(|(_, b)| *b).collect();
        let may_be_dn = !harmonized
            && units == "auto"
            && !needs_scale(&optical_nums)
            && crate::sensors::of_series(info).iter().any(|s| s.is_empty() || s.starts_with("LND"));
        let (conv_unit, conv_c2) = (format!("{target} * 10000"), format!("({target} * 2.75e-05 - 0.2) * 10000"));
        let assign_to = |e: &str| {
            if optical { format!("bands_cube.loc[dict(band=optical)] = {e}") } else { format!("bands_cube = {e}") }
        };
        match units.as_str() {
            "x10000" => {}
            "unit" if native => prep.scale = Some("1".into()),
            "auto" if native && !may_be_dn => prep.scale = Some("'auto'".into()),
            "unit" => {
                out.line("# Zeit takes reflectance x 10000; these are 0-1");
                out.line(assign_to(&conv_unit));
            }
            "landsat_c2" => {
                out.line("# Zeit takes reflectance x 10000; these are Landsat Collection 2 digital numbers");
                out.line(assign_to(&conv_c2));
            }
            _ => {
                out.line("# Zeit takes reflectance x 10000: Janus tells the units by the median (<= 2: 0-1; >= 6500: Landsat Collection 2 digital numbers)");
                out.line(format!("median = float({target}.median())"));
                out.line(assign_to(&format!(
                    "{conv_unit} if median <= 2 else {conv_c2} if median >= 6500 else {target}"
                )));
            }
        }
        // the quality band as Fmask codes
        let fmask_expr = qa_on.then(|| format!("zeit.qa_to_fmask(qa, \"{}\")", rule_arg(sel.qa_rule)));
        if fmask_expr.is_some() {
            zeit_321.set(true);
        }
        let fmask_users = banded.iter().filter(|c| c.inputs.contains(&"fmask")).count();
        let fmask_note = "as Fmask codes: 0 clear, 1 water, 2 shadow, 3 snow, 4 cloud, 255 none";
        if only_cd {
            // the tools take the codes themselves
            match fmask_expr {
                Some(e) if fmask_users > 1 => {
                    out.line(format!("fmask = {e}  # {} {fmask_note}", sel.qa_rule.label()));
                    prep.qa = Some(Some("fmask".into()));
                }
                Some(e) => prep.qa = Some(Some(e)),
                None => prep.qa = Some(None),
            }
        } else if fmask_users == 0 {
            // NDFI, CODED and Tmask use only the clear observations
            if let Some(e) = fmask_expr {
                out.line(format!(
                    "bands_cube = bands_cube.where({e} <= 1)  # only the clear observations (Fmask 0 and 1)"
                ));
            }
        } else {
            match fmask_expr {
                Some(e) => {
                    out.line(format!("fmask = {e}  # {} {fmask_note}", sel.qa_rule.label()));
                    out.line("fmask = fmask.where(bands_cube.notnull().all(\"band\"), 255)  # a date missing a band is no observation");
                }
                None => out.line(
                    "fmask = xr.where(bands_cube.notnull().all(\"band\"), 0, 255)  # no quality band: every complete observation is clear",
                ),
            }
            if banded.iter().any(|c| !c.inputs.contains(&"fmask")) {
                out.line("# NDFI, CODED and Tmask use only the clear observations: bands_cube.where(fmask <= 1)");
            }
        }
        // CODED and Tmask take a pixel's (time, band) cube as it is
        if pixel_mode && banded.iter().any(|c| matches!(c.tool.as_str(), "coded" | "tmask")) {
            zeit_321.set(true);
        }
        prep
    };
    let file_calls: Vec<usize> =
        (0..calls.len()).filter(|&i| calls[i].2.inputs.contains(&"bands_cube") && !harm_call[i]).collect();
    let file_prep = if file_calls.is_empty() { Prep::default() } else { prep(&file_calls, false, &mut body) };

    // ---- the tools
    let many = calls.len() > 1;
    let mut printed: Vec<String> = vec![];
    // (the result's variable, the file's stem) of every map a run writes
    let mut saves: Vec<(String, String)> = vec![];
    let mut tool_code = Code::default();
    let mut used: Vec<String> = vec![];
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
        used.push(call.tool.clone());
        let prefix = match (step, many) {
            (Some(_), _) if repeats > 0 => format!("{}{}_", call.tool, repeats + 1),
            (Some(_), _) | (None, true) => format!("{}_", call.tool),
            (None, false) => String::new(),
        };
        tool_code.blank();
        if let Some(s) = step {
            let reads = if s.reads_analysis {
                "the analysis series"
            } else if s.reads_harmonized {
                "the harmonized series"
            } else {
                "the series"
            };
            tool_code.line(format!("# {}. {name}, on {reads}", k + 1));
        } else if many {
            tool_code.line(format!("# {name}"));
        }
        // a step on the harmonized series makes its inputs of it, as the ones of the files are made above
        let mut prepared = file_prep.clone();
        if harm_call[i] {
            if call.inputs.contains(&"bands_cube") {
                tool_code.line("bands_cube = harmonized  # the harmonized series' bands, named by role");
                prepared = prep(&[i], true, &mut tool_code);
            }
            if call.inputs.contains(&"improphe_cubes") {
                tool_code.line("fine_cube = harmonized  # ImproPhe's fine series: the harmonized bands of each date");
                if qa_on {
                    new_zeit.set(true);
                    tool_code.line(format!(
                        "fine_cube = fine_cube.where(zeit.qa_mask(qa, \"{}\"))  # the dates the quality band refuses",
                        rule_arg(sel.qa_rule)
                    ));
                }
            }
        }
        let tweaked;
        let call = if call.inputs.contains(&"bands_cube") {
            let mut c = call.clone();
            if let Some(q) = &prepared.qa {
                c = c.with_arg("qa", q.as_deref());
            }
            if let Some(v) = &prepared.scale {
                c = c.with_arg("scale", Some(v));
            }
            tweaked = c;
            &tweaked
        } else {
            call
        };
        let (code, results) = call.compact(&prefix);
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
        if let (Some(s), Some(first)) = (step, results.first())
            && let Some(expr) = &s.harmonizes
        {
            tool_code.line(format!(
                "harmonized = {}  # the harmonized series: the steps below read its bands",
                expr.replace("{}", first)
            ));
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
        "\"\"\"Janus {} ({y:04}-{m:02}-{d:02}): {what} {} {}, reproduced outside Janus with Zeit.\nNeeds: pip install {}\n\"\"\"",
        spec.version,
        if run { "on" } else { "of" },
        spec.name,
        // what `load_raster` and the indices gained in Zeit 3.2: scale=, qa=, bands=, window=, pixel=, nd; and in 3.2.1
        // what the pin's code did by hand
        if calls.iter().any(|(_, _, c)| matches!(c.tool.as_str(), "mnf" | "cva" | "imad" | "radcal" | "omnibus" | "topocorr")) {
            ZEIT_WITH_CHANGE
        } else if calls.iter().any(|(_, _, c)| c.tool == "eof" || c.tool == "lagged_correlation") {
            ZEIT_WITH_EOF
        } else if zeit_321.get() {
            "\"zeit-cdts>=3.2.1\""
        } else if new_zeit.get() {
            "\"zeit-cdts>=3.2\""
        } else {
            "zeit-cdts"
        }
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
