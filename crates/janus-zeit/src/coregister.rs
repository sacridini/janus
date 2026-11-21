//! Co-registration between the dates of a series (Tools > Data preparation > Co-register dates), as Zeit's
//! `coregister` and `coregister_local`: ports of AROSICS 1.14.0 (Scheffler, Hollstein, Hofmann and Bochow 2017, Remote
//! Sensing 9(7):676; Apache-2.0). zeit-core finds the shifts (`coreg::coregister`: the phase correlation of one window;
//! `coreg_local::tie_points`: the same at the points of a grid, with AROSICS' filters); the warp is the caller's job,
//! and here it is GDAL's: each date's bands moved by their shift (the geotransform shifted, then warped onto the
//! window's grid) or by their tie points (ground control points, a thin plate spline or a polynomial), written as
//! warped VRTs (nothing computed until read) or as COGs.
//!
//! The matching band (NIR by default) of each date is compared with the reference's: a date of the series, the median
//! of some of its dates, or another open layer. The pixels the quality band refuses (and those without a value) are
//! left out of the windows, grown by a margin so the edges of a cloud stay out too. The quality band is moved by
//! nearest neighbour, the bands by the chosen resampling.
//!
//! A run writes the series (a file a date, the quality band kept as the last band), `shifts.csv` (a row a date),
//! `tie_points.csv` (local), the arrows as a GeoPackage (`shifts.gpkg`) and the report.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use gdal::raster::RasterCreationOptions;
use gdal::{Dataset, DriverManager};
use serde_json::json;
use zeit_core::coreg::{self, Failure, Image, Outcome};
use zeit_core::coreg_local::{self, Geo, LocalParams, TiePoint};

use crate::job::{Estimate, Inputs, JobResult, JobSpec, qa_usable};
use crate::manifest::Tool;
use crate::pixel::Params;
use crate::planes;
use crate::warp::{self, Format, Grid, WarpOptions};

/// A run needing more memory than this (bytes) is refused.
const MAX_BYTES: f64 = 6.0 * (1u64 << 30) as f64;
/// A window of more pixels than this is written as COGs when the format is left to the tool.
const COG_ABOVE: usize = 4000 * 4000;
/// The most ground control points GDAL's thin plate spline is given (AROSICS: "a GDAL bottleneck").
pub const MAX_GCPS: usize = 7000;
/// A date with a smaller share of clear pixels in the window is not matched.
const MIN_CLEAR: f64 = 0.05;
/// The least dates of a composite's median at a pixel.
const MIN_COMPOSITE: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reference {
    Date,
    Composite,
    Layer,
}

pub(crate) struct Settings {
    pub local: bool,
    pub reference: Reference,
    pub window_size: usize,
    pub max_shift: f64,
    pub max_iter: usize,
    pub min_reliability: f64,
    pub mask_buffer: usize,
    pub grid_res: usize,
    pub filter_level: u8,
    pub rs_max_outlier: f64,
    pub rs_tolerance: f64,
    pub min_points: usize,
    pub arosics_compat: bool,
    pub warp: String,
    pub resampling: String,
    pub arrow_scale: f64,
}

pub(crate) fn settings(p: &Params) -> Result<Settings, String> {
    let local = match p.str("mode") {
        "global" => false,
        "local" => true,
        other => return Err(format!("ValueError: mode must be 'global' or 'local', got {other:?}")),
    };
    let reference = match p.str("reference") {
        "date" => Reference::Date,
        "composite" => Reference::Composite,
        "layer" => Reference::Layer,
        other => return Err(format!("ValueError: reference must be 'date', 'composite' or 'layer', got {other:?}")),
    };
    let window_size = p.int("window_size");
    let (max_shift, max_iter) = (p.f64("max_shift"), p.int("max_iter"));
    if !max_shift.is_finite() || max_shift <= 0.0 || max_iter < 1 || window_size < 16 {
        return Err("ValueError: max_shift must be positive, max_iter at least 1 and window_size at least 16".into());
    }
    let filter_level = p.str("filter_level").parse::<u8>().ok().filter(|l| *l <= 3);
    let Some(filter_level) = filter_level else {
        return Err(format!("ValueError: the tie point filters must be 0, 1, 2 or 3, got {:?}", p.str("filter_level")));
    };
    let rs_max_outlier = p.f64("rs_max_outlier");
    if !(rs_max_outlier > 0.0 && rs_max_outlier < 100.0) {
        return Err("ValueError: rs_max_outlier must be between 0 and 100 (percent)".into());
    }
    let (grid_res, min_points) = (p.int("grid_res"), p.int("min_points"));
    if grid_res < 1 {
        return Err("ValueError: grid_res must be positive".into());
    }
    if min_points < 1 {
        return Err("ValueError: min_points must be at least 1".into());
    }
    let warp = p.str("warp").to_string();
    if !["tps", "poly1", "poly2", "poly3"].contains(&warp.as_str()) {
        return Err(format!("ValueError: warp must be 'tps' or 'poly1' to 'poly3', got {warp:?}"));
    }
    let resampling = p.str("resampling").to_string();
    if !["cubic", "bilinear", "lanczos", "near"].contains(&resampling.as_str()) {
        return Err(format!("ValueError: unknown resampling {resampling:?}"));
    }
    Ok(Settings {
        local,
        reference,
        window_size: window_size as usize,
        max_shift,
        max_iter: max_iter as usize,
        min_reliability: p.f64("min_reliability"),
        mask_buffer: p.int("mask_buffer").max(0) as usize,
        grid_res: grid_res as usize,
        filter_level,
        rs_max_outlier,
        rs_tolerance: p.f64("rs_tolerance"),
        min_points: min_points as usize,
        arosics_compat: p.bool("arosics_compat"),
        warp,
        resampling,
        arrow_scale: p.f64("arrow_scale"),
    })
}

/// The run's time: reading the matching band of each date, the matching, and the quality band's and bands' warps.
pub(crate) fn estimate(spec: &JobSpec, p: &Params, cells: f64) -> Result<Estimate, String> {
    let t = spec.years.len().max(1) as f64;
    let local = p.str("mode") == "local";
    let res = p.f64("grid_res").max(16.0);
    // a point's window is one FFT of the window's side with a few validations (about 4 ms at 256 pixels), on the
    // threads of the run
    let side = p.f64("window_size").max(16.0);
    let points = if local { (cells / (res * res)).max(1.0) } else { 1.0 };
    let per_point = 1.6e-8 * side * side * (side.log2() + 1.0) / 4.0;
    let threads = if local { 4.0 } else { 1.0 };
    Ok(Estimate {
        sec_per_px: t * 2e-8,
        sec_per_chunk: 0.0,
        chunk_cells: cells,
        sec_fixed: t * points * per_point / threads + 0.2 * t,
    })
}

// ---------------------------------------------------------------------------------------------------------------
// The images the matching reads
// ---------------------------------------------------------------------------------------------------------------

/// A window of one band: the values (0 where invalid) and whether each is data.
struct Img {
    data: Vec<f32>,
    valid: Vec<bool>,
}

impl Img {
    fn image(&self, w: usize, h: usize) -> Image<'_> {
        Image { data: &self.data, valid: &self.valid, ny: h, nx: w }
    }
}

/// `valid` with every pixel within `r` pixels (a square) of an invalid one made invalid: the margin of a cloud.
fn grow_invalid(valid: &mut [bool], w: usize, h: usize, r: usize) {
    if r == 0 || w == 0 || h == 0 {
        return;
    }
    let mut across = valid.to_vec();
    let mut pre = vec![0u32; w.max(h) + 1];
    for (row, out) in valid.chunks(w).zip(across.chunks_mut(w)) {
        for (x, &v) in row.iter().enumerate() {
            pre[x + 1] = pre[x] + u32::from(!v);
        }
        for (x, o) in out.iter_mut().enumerate() {
            *o = pre[(x + r + 1).min(w)] - pre[x.saturating_sub(r)] == 0;
        }
    }
    for x in 0..w {
        for y in 0..h {
            pre[y + 1] = pre[y] + u32::from(!across[y * w + x]);
        }
        for (y, o) in valid.iter_mut().skip(x).step_by(w).enumerate() {
            *o = pre[(y + r + 1).min(h)] - pre[y.saturating_sub(r)] == 0;
        }
    }
}

/// One date's matching band over the window: its values, what is data (finite and kept by the quality band), the
/// share of the window that is, and the validity grown by `buffer` pixels.
fn read_date(
    inputs: &mut Inputs,
    role: &str,
    k: usize,
    window: [usize; 4],
    buffer: usize,
) -> Result<(Img, f64), String> {
    let [x0, y0, x1, y1] = window;
    let (w, h) = (x1 - x0, y1 - y0);
    let values = inputs.read_dates_with(&[role.to_string()], &[k], x0, y0, w, h, false)?.swap_remove(0);
    let codes = inputs.qa_codes(&[k], x0, y0, w, h)?.map(|mut c| c.swap_remove(0));
    let rule = inputs.qa_rule().map(str::to_string);
    let mut valid: Vec<bool> = values
        .iter()
        .enumerate()
        .map(|(i, v)| v.is_finite() && codes.as_ref().is_none_or(|c| qa_usable(rule.as_deref(), c[i])))
        .collect();
    let share = valid.iter().filter(|&&v| v).count() as f64 / valid.len().max(1) as f64;
    grow_invalid(&mut valid, w, h, buffer);
    let data = values.iter().zip(&valid).map(|(&v, &ok)| if ok { v as f32 } else { 0.0 }).collect();
    Ok((Img { data, valid }, share))
}

/// The share of the window a date has clear: by its quality band when it has one (cheap), else by the band.
fn clear_share(inputs: &mut Inputs, role: &str, k: usize, window: [usize; 4]) -> Result<f64, String> {
    let [x0, y0, x1, y1] = window;
    let (w, h) = (x1 - x0, y1 - y0);
    let rule = inputs.qa_rule().map(str::to_string);
    let n = (w * h).max(1) as f64;
    if let Some(mut codes) = inputs.qa_codes(&[k], x0, y0, w, h)? {
        let codes = codes.swap_remove(0);
        return Ok(codes.iter().filter(|&&c| qa_usable(rule.as_deref(), c)).count() as f64 / n);
    }
    let v = inputs.read_dates_with(&[role.to_string()], &[k], x0, y0, w, h, false)?.swap_remove(0);
    Ok(v.iter().filter(|x| x.is_finite()).count() as f64 / n)
}

/// The median of the values (the mean of the two middle ones for an even count, as numpy's).
fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 { v[n / 2] } else { 0.5 * (v[n / 2 - 1] + v[n / 2]) }
}

/// The median of `dates`' matching band at each pixel, over the clear observations; a pixel with fewer than
/// `MIN_COMPOSITE` is invalid. Read by strips of rows.
fn composite(
    inputs: &mut Inputs,
    role: &str,
    dates: &[usize],
    window: [usize; 4],
    cancel: &AtomicBool,
) -> Result<Img, String> {
    let [x0, y0, x1, y1] = window;
    let (w, h) = (x1 - x0, y1 - y0);
    let rule = inputs.qa_rule().map(str::to_string);
    let strip = planes::strip_rows(dates.len(), 1, w, h);
    let mut data = vec![0f32; w * h];
    let mut valid = vec![false; w * h];
    let mut stack: Vec<f64> = Vec::with_capacity(dates.len());
    let mut r = 0;
    while r < h {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let hh = strip.min(h - r);
        let np = w * hh;
        let planes_in = inputs.read_dates_with(&[role.to_string()], dates, x0, y0 + r, w, hh, false)?;
        let codes = inputs.qa_codes(dates, x0, y0 + r, w, hh)?;
        for p in 0..np {
            stack.clear();
            for (d, plane) in planes_in.iter().enumerate() {
                let kept = codes.as_ref().is_none_or(|c| qa_usable(rule.as_deref(), c[d][p]));
                if plane[p].is_finite() && kept {
                    stack.push(plane[p]);
                }
            }
            if stack.len() >= MIN_COMPOSITE {
                data[r * w + p] = median(&mut stack) as f32;
                valid[r * w + p] = true;
            }
        }
        r += hh;
    }
    Ok(Img { data, valid })
}

static SCRATCH: AtomicUsize = AtomicUsize::new(0);

/// Another layer's band on the window's grid: its date `date` (1-based; 0: the median of all its dates), warped
/// by cubic convolution whatever its grid. Whether its grid was the window's is returned too.
fn layer_reference(
    spec: &JobSpec,
    set_band: &str,
    ref_band: i64,
    ref_date: i64,
    grid: &Grid,
    threads: usize,
    cancel: &AtomicBool,
) -> Result<(Img, bool, String), String> {
    let other = spec
        .other
        .as_ref()
        .ok_or("ValueError: choose the reference layer (another open series or map, in the same CRS as this one)")?;
    if other.inputs.is_empty() || other.years.is_empty() {
        return Err("ValueError: the reference layer has no bands or dates".into());
    }
    let band = if ref_band > 0 {
        let b = ref_band as usize - 1;
        if b >= other.inputs.len() {
            return Err(format!(
                "ValueError: the reference layer has {} band(s) a date, not {ref_band}",
                other.inputs.len()
            ));
        }
        b
    } else {
        other
            .band_names
            .iter()
            .position(|n| !n.is_empty() && n.eq_ignore_ascii_case(set_band))
            .filter(|&b| b < other.inputs.len())
            .unwrap_or(0)
    };
    let dates: Vec<usize> = if ref_date > 0 {
        let d = ref_date as usize;
        if d > other.years.len() {
            return Err(format!("ValueError: the reference layer has {} date(s), not {d}", other.years.len()));
        }
        vec![d]
    } else {
        (1..=other.years.len()).collect()
    };
    let path = &other.inputs[band];
    let ds = Dataset::open(path).map_err(|e| format!("{path}: {e}"))?;
    let (sw, sh) = ds.raster_size();
    let gt = ds.geo_transform().map_err(|e| format!("ValueError: the reference layer has no geotransform ({e})"))?;
    let wkt = ds.projection();
    if wkt.is_empty() {
        return Err("ValueError: the reference layer has no CRS".into());
    }
    let same = (sw, sh) == (grid.w, grid.h)
        && gt.iter().zip(&grid.gt).all(|(a, b)| (a - b).abs() <= 1e-6 * grid.gt[1].abs())
        && crate::improphe::same_crs(&wkt, &grid.wkt).unwrap_or(false);
    let (w, h) = (grid.w, grid.h);
    let mut planes_in: Vec<Vec<f64>> = vec![];
    for d in dates {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let id = SCRATCH.fetch_add(1, Ordering::Relaxed);
        let tag = format!("{}_{id}", std::process::id());
        // the VRT on disk: a /vsimem/ file made from a Rust buffer would be freed by GDAL with the C runtime's free,
        // which corrupts the heap of janus.exe (mimalloc)
        let one_file = std::env::temp_dir().join(format!("janus_coreg_ref_{tag}.vrt"));
        let one = one_file.to_string_lossy().into_owned();
        let out = format!("/vsimem/janus_coreg_ref_{tag}.tif");
        let band_in = VBand { file: path, band: d, name: String::new(), nodata_nan: true };
        write_vrt(&one, &VrtGrid { w: sw, h: sh, wkt: &wkt, gt: Some(gt), gcps: &[] }, &[band_in], [0, 0, sw, sh])?;
        let opts = WarpOptions {
            grid: Some(grid.clone()),
            resampling: "cubic".into(),
            dst_nodata: Some(f64::NAN),
            data_type: Some("Float32".into()),
            format: Format::GTiff,
            threads,
            ..Default::default()
        };
        let done = warp::warp(&one, &out, &opts, &|_| !cancel.load(Ordering::Relaxed));
        let _ = std::fs::remove_file(&one_file);
        done.map_err(|e| format!("the reference layer: {e}"))?;
        let ds = Dataset::open(&out).map_err(|e| format!("{out}: {e}"))?;
        let v = crate::job::read(&ds, 1..=1, 0, 0, w, h, false)?;
        drop(ds);
        let _ = gdal::vsi::unlink_mem_file(&out);
        planes_in.push(v);
    }
    let mut data = vec![0f32; w * h];
    let mut valid = vec![false; w * h];
    let mut stack: Vec<f64> = Vec::with_capacity(planes_in.len());
    for p in 0..w * h {
        stack.clear();
        stack.extend(planes_in.iter().map(|v| v[p]).filter(|v| v.is_finite()));
        if !stack.is_empty() {
            data[p] = median(&mut stack) as f32;
            valid[p] = true;
        }
    }
    let name =
        other.band_names.get(band).filter(|n| !n.is_empty()).cloned().unwrap_or_else(|| format!("band {}", band + 1));
    Ok((Img { data, valid }, same, name))
}

/// The role of the band whose texture is matched, and its name: the one asked for, or (automatic) the NIR, else the
/// red, else the first band.
fn match_role(spec: &JobSpec, want: &str) -> Result<(String, String), String> {
    let name_of = |role: &str| {
        if role.starts_with('b') && role[1..].bytes().all(|c| c.is_ascii_digit()) {
            planes::name_of(spec, role)
        } else {
            role.to_string()
        }
    };
    if want != "auto" {
        return if spec.bands.contains_key(want) {
            Ok((want.to_string(), want.to_string()))
        } else {
            Err(format!(
                "ValueError: the band {want} is not mapped: map it in the series' bands, or choose another band to match"
            ))
        };
    }
    for role in ["nir", "red"] {
        if spec.bands.contains_key(role) {
            return Ok((role.to_string(), role.to_string()));
        }
    }
    let first = spec
        .bands
        .keys()
        .find(|k| k.len() >= 4 && k.starts_with('b') && k[1..].bytes().all(|c| c.is_ascii_digit()))
        .ok_or("ValueError: the series has no band to match")?;
    Ok((first.clone(), name_of(first)))
}

/// The code the quality band takes where a date has no data (a pixel the warp brings in from outside), as its rule
/// refuses it.
fn fill_code(rule: Option<&str>) -> f64 {
    match rule {
        Some("fmask" | "hls_fmask") => 255.0,
        Some("landsat_c2") => 1.0,
        _ => 0.0,
    }
}

// ---------------------------------------------------------------------------------------------------------------
// VRTs
// ---------------------------------------------------------------------------------------------------------------

/// A band of a VRT: band `band` (1-based) of `file`, over `rect` of it.
struct VBand<'a> {
    file: &'a str,
    band: usize,
    name: String,
    nodata_nan: bool,
}

/// What a VRT sits on: its size, CRS and geotransform, or ground control points instead of a geotransform.
struct VrtGrid<'a> {
    w: usize,
    h: usize,
    wkt: &'a str,
    gt: Option<[f64; 6]>,
    /// (column, row, x, y) of each ground control point
    gcps: &'a [[f64; 4]],
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// A number as GDAL reads it back exactly.
fn num(v: f64) -> String {
    format!("{v:?}")
}

/// Writes a VRT of Float32 bands, each a SimpleSource over `rect` (x, y, w, h) of its file, laid on the whole VRT.
/// Never under /vsimem/ from a Rust buffer (GDAL would free it with the C runtime's free: heap corruption under
/// mimalloc).
fn write_vrt(path: &str, grid: &VrtGrid, bands: &[VBand], rect: [usize; 4]) -> Result<(), String> {
    let mut x = format!("<VRTDataset rasterXSize=\"{}\" rasterYSize=\"{}\">\n", grid.w, grid.h);
    if let Some(g) = grid.gt {
        if !grid.wkt.is_empty() {
            x += &format!("  <SRS>{}</SRS>\n", xml(grid.wkt));
        }
        x += &format!("  <GeoTransform>{}</GeoTransform>\n", g.iter().map(|v| num(*v)).collect::<Vec<_>>().join(", "));
    }
    if !grid.gcps.is_empty() {
        x += &format!("  <GCPList Projection=\"{}\">\n", xml(grid.wkt));
        for (i, [col, row, gx, gy]) in grid.gcps.iter().enumerate() {
            x += &format!(
                "    <GCP Id=\"{}\" Pixel=\"{}\" Line=\"{}\" X=\"{}\" Y=\"{}\" Z=\"0\"/>\n",
                i + 1,
                num(*col),
                num(*row),
                num(*gx),
                num(*gy)
            );
        }
        x += "  </GCPList>\n";
    }
    for (i, b) in bands.iter().enumerate() {
        x += &format!("  <VRTRasterBand dataType=\"Float32\" band=\"{}\">\n", i + 1);
        if !b.name.is_empty() {
            x += &format!("    <Description>{}</Description>\n", xml(&b.name));
        }
        if b.nodata_nan {
            x += "    <NoDataValue>nan</NoDataValue>\n";
        }
        x += "    <SimpleSource>\n";
        x += &format!("      <SourceFilename relativeToVRT=\"0\">{}</SourceFilename>\n", xml(b.file));
        x += &format!("      <SourceBand>{}</SourceBand>\n", b.band);
        x += &format!(
            "      <SrcRect xOff=\"{}\" yOff=\"{}\" xSize=\"{}\" ySize=\"{}\"/>\n",
            rect[0], rect[1], rect[2], rect[3]
        );
        x += &format!("      <DstRect xOff=\"0\" yOff=\"0\" xSize=\"{}\" ySize=\"{}\"/>\n", grid.w, grid.h);
        x += "    </SimpleSource>\n";
        x += "  </VRTRasterBand>\n";
    }
    x += "</VRTDataset>\n";
    std::fs::write(path, x).map_err(|e| format!("{path}: {e}"))
}

/// (file, band) of each band of a file.
type FileBands = Vec<(String, usize)>;

/// The bands of a VRT from (file, band, name, has NaN as NoData) lists.
fn as_bands(v: &[(String, usize, String, bool)]) -> Vec<VBand<'_>> {
    v.iter().map(|(f, b, n, nan)| VBand { file: f.as_str(), band: *b, name: n.clone(), nodata_nan: *nan }).collect()
}

/// What moves a date onto the reference.
enum Motion {
    /// as it is
    Stay,
    /// its content moved by (x, y) pixels, x to the right and y down
    Shift(f64, f64),
    /// ground control points: (column, row, x, y) of a pixel and the map position it must take
    Points(Vec<[f64; 4]>),
}

/// The files of a run's series: the sources the VRTs of the dates read, and where they go.
struct Writer<'a> {
    window: [usize; 4],
    grid: &'a Grid,
    /// the bands' VRTs (each of the dates, one band a date) and the quality band's
    band_files: Vec<String>,
    qa_file: Option<String>,
    names: Vec<String>,
    qa_name: String,
    qa_rule: Option<String>,
    parts: PathBuf,
    series: PathBuf,
    resampling: String,
    /// a polynomial's order for a date warped by tie points (None: a thin plate spline)
    order: Option<u8>,
    cog: bool,
    threads: usize,
}

impl Writer<'_> {
    /// Date `k`'s file in the series' folder, made of its bands (warped by `motion`) and its quality band (moved by
    /// nearest neighbour).
    fn date(&self, k: usize, stem: &str, motion: &Motion, cancel: &AtomicBool) -> Result<PathBuf, String> {
        let (w, h) = (self.grid.w, self.grid.h);
        let [x0, y0, ..] = self.window;
        let rect = [x0, y0, w, h];
        let step = |_: f64| !cancel.load(Ordering::Relaxed);
        let part = |suffix: &str| self.parts.join(format!("{stem}{suffix}.vrt")).to_string_lossy().into_owned();
        let sources = |files: &[String], names: &[String], nan: bool| -> Vec<(String, usize, String, bool)> {
            files.iter().zip(names).map(|(f, n)| (f.clone(), k + 1, n.clone(), nan)).collect()
        };
        let band_src = sources(&self.band_files, &self.names, true);
        let qa_src = self.qa_file.iter().map(|f| (f.clone(), k + 1, self.qa_name.clone(), false)).collect::<Vec<_>>();
        // (file, band) of each band of the date's final file: the sources as they are, or what the warps made
        let (final_bands, final_qa): (FileBands, FileBands);
        let mut final_rect = rect;
        match motion {
            Motion::Stay => {
                final_bands = band_src.iter().map(|(f, b, ..)| (f.clone(), *b)).collect();
                final_qa = qa_src.iter().map(|(f, b, ..)| (f.clone(), *b)).collect();
            }
            moving => {
                let (shifted, gcps): (Option<[f64; 6]>, &[[f64; 4]]) = match moving {
                    Motion::Shift(dx, dy) => {
                        let g = self.grid.gt;
                        (Some([g[0] + dx * g[1], g[1], 0.0, g[3] + dy * g[5], 0.0, g[5]]), &[])
                    }
                    Motion::Points(p) => (None, p),
                    Motion::Stay => (None, &[]),
                };
                let by_points = matches!(moving, Motion::Points(_));
                let (tps, order) = (by_points && self.order.is_none(), self.order.filter(|_| by_points));
                let source = VrtGrid { w, h, wkt: &self.grid.wkt, gt: shifted, gcps };
                let mut opts = WarpOptions {
                    grid: Some(self.grid.clone()),
                    resampling: self.resampling.clone(),
                    src_nodata: Some(f64::NAN),
                    dst_nodata: Some(f64::NAN),
                    data_type: Some("Float32".into()),
                    format: Format::Vrt,
                    threads: self.threads,
                    tps,
                    order,
                    ..Default::default()
                };
                let (src_path, out_path) = (part("_src"), part("_bands"));
                write_vrt(&src_path, &source, &as_bands(&band_src), rect)?;
                warp::warp(&src_path, &out_path, &opts, &step)?;
                final_bands = (1..=band_src.len()).map(|b| (out_path.clone(), b)).collect();
                final_rect = [0, 0, w, h];
                if !qa_src.is_empty() {
                    let (qa_path, qa_out) = (part("_qa_src"), part("_qa"));
                    write_vrt(&qa_path, &source, &as_bands(&qa_src), rect)?;
                    opts.resampling = "near".into();
                    // the codes are all values: no NoData (GDAL would move a code equal to it), the cells brought in from
                    // outside start as the rule's refused code
                    opts.src_nodata = None;
                    opts.dst_nodata = None;
                    opts.init_dest = Some(fill_code(self.qa_rule.as_deref()));
                    warp::warp(&qa_path, &qa_out, &opts, &step)?;
                    final_qa = vec![(qa_out, 1)];
                } else {
                    final_qa = vec![];
                }
            }
        }
        // the file of the date: the bands, then the quality band, on the window's grid
        let mut list: Vec<VBand> = final_bands
            .iter()
            .zip(&self.names)
            .map(|((f, b), n)| VBand { file: f, band: *b, name: n.clone(), nodata_nan: true })
            .collect();
        let qa_bands: Vec<VBand> = final_qa
            .iter()
            .map(|(f, b)| VBand { file: f, band: *b, name: self.qa_name.clone(), nodata_nan: false })
            .collect();
        list.extend(qa_bands);
        let target = VrtGrid { w, h, wkt: &self.grid.wkt, gt: Some(self.grid.gt), gcps: &[] };
        let final_vrt =
            if self.cog { part("") } else { self.series.join(format!("{stem}.vrt")).to_string_lossy().into_owned() };
        write_vrt(&final_vrt, &target, &list, final_rect)?;
        if !self.cog {
            return Ok(PathBuf::from(final_vrt));
        }
        let out = self.series.join(format!("{stem}.tif"));
        let src = Dataset::open(&final_vrt).map_err(|e| format!("{final_vrt}: {e}"))?;
        let driver = DriverManager::get_driver_by_name("COG").map_err(|e| e.to_string())?;
        let options = RasterCreationOptions::from_iter([
            "COMPRESS=DEFLATE".to_string(),
            "PREDICTOR=YES".into(),
            "BIGTIFF=IF_SAFER".into(),
            format!("NUM_THREADS={}", self.threads.max(1)),
        ]);
        src.create_copy(&driver, &out, &options).map_err(|e| format!("{}: {e}", out.display()))?;
        Ok(out)
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The tables
// ---------------------------------------------------------------------------------------------------------------

/// A date's row of `shifts.csv`.
#[derive(Clone, Debug, Default)]
struct DateRow {
    label: String,
    x_shift: f64,
    y_shift: f64,
    reliability: f64,
    ssim_before: f64,
    ssim_after: f64,
    success: bool,
    reason: String,
    window: Option<[usize; 4]>,
    fft_size: usize,
    applied: bool,
    counts: Option<[usize; 3]>,
    valid_share: f64,
    is_reference: bool,
}

const SHIFT_HEADER: [&str; 21] = [
    "date",
    "x_shift_px",
    "y_shift_px",
    "x_shift_map",
    "y_shift_map",
    "reliability",
    "ssim_before",
    "ssim_after",
    "success",
    "reason",
    "window_x0",
    "window_y0",
    "window_x1",
    "window_y1",
    "fft_size",
    "applied",
    "points",
    "matched",
    "used",
    "valid_share",
    "reference",
];

const POINT_HEADER: [&str; 28] = [
    "date",
    "point_id",
    "col",
    "row",
    "x",
    "y",
    "x_shift_px",
    "y_shift_px",
    "x_shift_map",
    "y_shift_map",
    "abs_shift",
    "angle",
    "reliability",
    "ssim_before",
    "ssim_after",
    "ssim_improved",
    "success",
    "reason",
    "win_size_x",
    "win_size_y",
    "fft_size",
    "outlier_reliability",
    "outlier_ssim",
    "outlier_ransac",
    "outlier",
    "used",
    "date_applied",
    "valid_share",
];

fn shift_rows(rows: &[DateRow], cell: [f64; 2], x0: usize, y0: usize) -> Vec<Vec<String>> {
    let n = planes::csv_num;
    rows.iter()
        .map(|r| {
            let win = |i: usize| match r.window {
                Some(w) if w[2] > w[0] => (if i.is_multiple_of(2) { x0 } else { y0 } + w[i]).to_string(),
                _ => String::new(),
            };
            let count = |i: usize| r.counts.map_or(String::new(), |c| c[i].to_string());
            vec![
                r.label.clone(),
                n(r.x_shift),
                n(r.y_shift),
                n(r.x_shift * cell[0]),
                n(r.y_shift * cell[1]),
                n(r.reliability),
                n(r.ssim_before),
                n(r.ssim_after),
                r.success.to_string(),
                r.reason.clone(),
                win(0),
                win(1),
                win(2),
                win(3),
                if r.fft_size > 0 { r.fft_size.to_string() } else { String::new() },
                r.applied.to_string(),
                count(0),
                count(1),
                count(2),
                n(r.valid_share),
                r.is_reference.to_string(),
            ]
        })
        .collect()
}

/// An arrow of the shifts layer: from where a shift was measured to where it points, magnified.
struct Arrow {
    date: String,
    from: [f64; 2],
    to: [f64; 2],
    dx_px: f64,
    dy_px: f64,
    dx_map: f64,
    dy_map: f64,
    reliability: f64,
    used: bool,
    reason: String,
}

/// The arrows as a GeoPackage (layer "shifts") in the series' CRS.
fn write_arrows(path: &Path, wkt: &str, arrows: &[Arrow]) -> Result<(), String> {
    use gdal::vector::{FieldDefn, Geometry, LayerAccess, LayerOptions, OGRFieldType, OGRwkbGeometryType};
    let _ = std::fs::remove_file(path);
    let drv = DriverManager::get_driver_by_name("GPKG").map_err(|e| e.to_string())?;
    let mut ds = drv.create_vector_only(path).map_err(|e| e.to_string())?;
    let srs = (!wkt.is_empty()).then(|| gdal::spatial_ref::SpatialRef::from_wkt(wkt).ok()).flatten();
    let layer = ds
        .create_layer(LayerOptions {
            name: "shifts",
            srs: srs.as_ref(),
            ty: OGRwkbGeometryType::wkbLineString,
            options: None,
        })
        .map_err(|e| e.to_string())?;
    let fields = [
        ("date", OGRFieldType::OFTString),
        ("dx_px", OGRFieldType::OFTReal),
        ("dy_px", OGRFieldType::OFTReal),
        ("dx_map", OGRFieldType::OFTReal),
        ("dy_map", OGRFieldType::OFTReal),
        ("reliability", OGRFieldType::OFTReal),
        ("used", OGRFieldType::OFTInteger),
        ("reason", OGRFieldType::OFTString),
    ];
    for (name, ty) in fields {
        FieldDefn::new(name, ty).and_then(|d| d.add_to_layer(&layer)).map_err(|e| e.to_string())?;
    }
    for a in arrows {
        let g = Geometry::from_wkt(&format!(
            "LINESTRING ({} {}, {} {})",
            num(a.from[0]),
            num(a.from[1]),
            num(a.to[0]),
            num(a.to[1])
        ))
        .map_err(|e| e.to_string())?;
        let mut f = gdal::vector::Feature::new(layer.defn()).map_err(|e| e.to_string())?;
        f.set_geometry(g).map_err(|e| e.to_string())?;
        f.set_field_string(0, &a.date).map_err(|e| e.to_string())?;
        for (i, v) in [a.dx_px, a.dy_px, a.dx_map, a.dy_map, a.reliability].into_iter().enumerate() {
            if v.is_finite() {
                f.set_field_double(i + 1, v).map_err(|e| e.to_string())?;
            }
        }
        f.set_field_integer(6, i32::from(a.used)).map_err(|e| e.to_string())?;
        f.set_field_string(7, &a.reason).map_err(|e| e.to_string())?;
        f.create(&layer).map_err(|e| e.to_string())?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------------------------
// The matching
// ---------------------------------------------------------------------------------------------------------------

/// `np.arange(0, size + res, res)`: the corners of the cells of a grid of tie points, from the first one to the one
/// past the last.
fn grid_points(size: usize, res: usize) -> Vec<f64> {
    (0..).map(|k| (k * res) as f64).take_while(|&x| x < (size + res) as f64).collect()
}

/// The ground control points of a date's used tie points (at most `MAX_GCPS`, spread evenly over the list).
fn control_points(points: &[&TiePoint], cols_rows: &[(f64, f64)]) -> Vec<[f64; 4]> {
    let all: Vec<[f64; 4]> = points
        .iter()
        .zip(cols_rows)
        .map(|(t, &(c, r))| [c, r, t.x_map + t.x_shift_map, t.y_map + t.y_shift_map])
        .collect();
    spread(all)
}

/// At most `MAX_GCPS` of the points, spread evenly over the list (`np.linspace(0, n - 1, MAX_GCPS).round()`).
fn spread(all: Vec<[f64; 4]>) -> Vec<[f64; 4]> {
    if all.len() <= MAX_GCPS {
        return all;
    }
    (0..MAX_GCPS).map(|j| all[((j as f64) * (all.len() - 1) as f64 / (MAX_GCPS - 1) as f64).round() as usize]).collect()
}

/// The centre and side of the largest square of pixels valid in both images (pixel coordinates, the corner of the first
/// pixel is the origin): where a matching window can sit when a cloud is in the middle of the overlap.
fn largest_valid_square(a: &[bool], b: &[bool], w: usize, h: usize) -> Option<(f64, f64, usize)> {
    let mut above = vec![0u32; w + 1];
    let mut best = (0u32, 0usize, 0usize);
    for y in 0..h {
        let mut diagonal = 0u32;
        let mut left = 0u32;
        for x in 0..w {
            let ok = a[y * w + x] && b[y * w + x];
            let here = if ok { 1 + diagonal.min(above[x + 1]).min(left) } else { 0 };
            diagonal = above[x + 1];
            above[x + 1] = here;
            left = here;
            if here > best.0 {
                best = (here, x, y);
            }
        }
    }
    let (side, x, y) = best;
    (side > 0).then(|| (x as f64 + 1.0 - f64::from(side) / 2.0, y as f64 + 1.0 - f64::from(side) / 2.0, side as usize))
}

/// The shift of a date to the reference. AROSICS puts its window at the middle of what the images share and grows it
/// from there until it meets a pixel without data, so a cloud near the middle leaves it small, or none at all. Where
/// the window cannot be placed or comes out under three quarters of the side a clear square of the images would give,
/// it is tried again at the largest clear square (when that holds a window of 32 pixels or more), and that answer is
/// kept if it succeeded and is more reliable; the answer says whether it was.
fn match_global(reference: &Img, target: &Img, w: usize, h: usize, p: &coreg::Params) -> (Outcome, bool) {
    let first = coreg::coregister(&reference.image(w, h), &target.image(w, h), p);
    let side = |o: &Outcome| (o.window[2].saturating_sub(o.window[0])).min(o.window[3].saturating_sub(o.window[1]));
    let unplaced = matches!(first.failure, Some(Failure::WindowTooSmall | Failure::WindowShrink));
    let cut = first.window[2] > first.window[0] && side(&first) < p.window_size;
    if !unplaced && !cut {
        return (first, false);
    }
    let Some((cx, cy, clear)) = largest_valid_square(&reference.valid, &target.valid, w, h).filter(|s| s.2 >= 32)
    else {
        return (first, false);
    };
    // the window the clear square would hold, against the one the first try had
    if !unplaced && 4 * side(&first) >= 3 * p.window_size.min(clear) {
        return (first, false);
    }
    let moved = coreg::Params { window_pos: Some((cx, cy)), window_size: p.window_size.min(clear & !1), ..p.clone() };
    let second = coreg::coregister(&reference.image(w, h), &target.image(w, h), &moved);
    let better = second.success && (!first.success || second.reliability >= first.reliability);
    if better { (second, true) } else { (first, false) }
}

/// A date that could not be matched, as a row.
fn unmatched(label: &str, why: String, share: f64) -> DateRow {
    DateRow {
        label: label.to_string(),
        x_shift: f64::NAN,
        y_shift: f64::NAN,
        reliability: f64::NAN,
        ssim_before: f64::NAN,
        ssim_after: f64::NAN,
        reason: why,
        valid_share: share,
        ..Default::default()
    }
}

fn clouded(share: f64) -> String {
    format!("too cloudy: {:.1}% of the window is clear, under {:.0}%", share * 100.0, MIN_CLEAR * 100.0)
}

pub(crate) fn run(
    spec: &JobSpec,
    _tool: &Tool,
    p: &Params,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<JobResult, String> {
    let set = settings(p)?;
    let bands = planes::band_set(spec, "", 1, "the co-registration")?;
    let t = spec.years.len();
    let (role, role_name) = match_role(spec, p.str("band"))?;
    let mut inputs = Inputs::new(spec)?;
    let window = planes::window_of(spec, &inputs);
    let [x0, y0, x1, y1] = window;
    let (w, h) = (x1 - x0, y1 - y0);
    let n = w * h;
    let (grid, _) = crate::topocorr::window_grid(inputs.input.ds(), window)?;
    let cell = [grid.gt[1].abs(), grid.gt[5].abs()];
    let nb = bands.roles.len();
    let bytes = n as f64 * 26.0;
    if bytes > MAX_BYTES {
        return Err(format!(
            "ValueError: the window ({w} x {h} pixels) needs about {:.1} GB of memory, over the {:.0} GB limit: choose the ROI or the visible area",
            bytes / (1u64 << 30) as f64,
            MAX_BYTES / (1u64 << 30) as f64
        ));
    }
    if w < 32 || h < 32 {
        return Err(format!("ValueError: the window ({w} x {h} pixels) is too small to match"));
    }
    // the VRTs of the series name their sources by absolute paths
    let dir = std::path::absolute(&spec.output_dir).unwrap_or_else(|_| PathBuf::from(&spec.output_dir));
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let labels = crate::job::date_labels(spec);
    let n_jobs = spec.n_jobs();
    let threads = match n_jobs {
        k if k > 0 => k as usize,
        _ => std::thread::available_parallelism().map_or(4, |k| k.get()),
    }
    .clamp(1, 8);
    let mut notes: Vec<String> = vec![];

    // ---- the reference
    progress(0.0, "the reference");
    let mut ref_index: Option<usize> = None;
    let (reference, ref_text): (Img, String) = match set.reference {
        Reference::Date => {
            let k = match p.int("ref_date") {
                0 => {
                    let mut best = (0usize, -1.0f64);
                    for d in 0..t {
                        if cancel.load(Ordering::Relaxed) {
                            return Err("cancelled".into());
                        }
                        progress(0.04 * d as f64 / t.max(1) as f64, "counting the clear pixels");
                        let share = clear_share(&mut inputs, &role, d, window)?;
                        if share > best.1 {
                            best = (d, share);
                        }
                    }
                    best.0
                }
                d if (1..=t as i64).contains(&d) => d as usize - 1,
                d => return Err(format!("ValueError: the reference date {d} is not one of the {t} dates")),
            };
            ref_index = Some(k);
            let (img, share) = read_date(&mut inputs, &role, k, window, set.mask_buffer)?;
            if share < MIN_CLEAR {
                return Err(format!("ValueError: the reference date {} is {}", labels[k], clouded(share)));
            }
            (img, format!("the date {} ({})", labels[k], k + 1))
        }
        Reference::Composite => {
            let dates: Vec<usize> = if p.str("ref_dates") == "all" || spec.sensors.len() != t {
                (0..t).collect()
            } else {
                let mut count: std::collections::BTreeMap<&str, usize> = Default::default();
                for s in spec.sensors.iter().filter(|s| !s.is_empty()) {
                    *count.entry(s.as_str()).or_default() += 1;
                }
                match count.iter().max_by_key(|(_, c)| **c).map(|(s, _)| *s) {
                    Some(sensor) => (0..t).filter(|&d| spec.sensors[d] == sensor).collect(),
                    None => (0..t).collect(),
                }
            };
            if dates.len() < MIN_COMPOSITE {
                return Err(format!(
                    "ValueError: a composite reference needs at least {MIN_COMPOSITE} dates, {} chosen",
                    dates.len()
                ));
            }
            let img = composite(&mut inputs, &role, &dates, window, cancel)?;
            if img.valid.iter().filter(|&&v| v).count() < n / 20 {
                return Err("ValueError: the composite reference has almost no pixel clear on three dates".into());
            }
            (img, format!("the median of {} dates", dates.len()))
        }
        Reference::Layer => {
            let (img, same, name) =
                layer_reference(spec, &role_name, p.int("ref_band"), p.int("ref_layer_date"), &grid, threads, cancel)?;
            let name = spec.other.as_ref().map_or(String::new(), |o| format!("{} ({name})", o.name));
            if !same {
                notes
                    .push(format!("The reference layer {name} was resampled to the series' grid (cubic convolution)."));
            }
            (img, format!("the layer {name}"))
        }
    };
    if reference.valid.iter().all(|v| !*v) {
        return Err("ValueError: the reference has no valid data in the window".into());
    }
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }

    // ---- the shifts
    let coreg_params = coreg::Params {
        window_size: set.window_size,
        window_pos: None,
        max_shift: set.max_shift,
        max_iter: set.max_iter,
        integer_range: None,
        nodata: None,
        fill: 0.0,
        cell,
    };
    let local_params = LocalParams {
        coreg: coreg_params.clone(),
        filter_level: set.filter_level,
        min_reliability: if set.min_reliability > 0.0 { set.min_reliability } else { 60.0 },
        rs_max_outlier: set.rs_max_outlier,
        rs_tolerance: set.rs_tolerance,
        rs_max_iter: 15,
        rs_seed: Some(0),
        rs_arosics_quirk: set.arosics_compat,
    };
    let geo = Geo { x0: grid.gt[0], dx: grid.gt[1], y0: grid.gt[3], dy: grid.gt[5] };
    let points: Vec<(f64, f64)> = {
        let (xs, ys) = (grid_points(w, set.grid_res), grid_points(h, set.grid_res));
        ys.iter().flat_map(|&y| xs.iter().map(move |&x| (x, y))).collect()
    };
    let mut rows: Vec<DateRow> = vec![];
    let mut motions: Vec<Motion> = (0..t).map(|_| Motion::Stay).collect();
    let mut point_rows: Vec<Vec<String>> = vec![];
    let mut arrows: Vec<Arrow> = vec![];
    let batch = threads.min(((1.5e9 / (n as f64 * 9.0)) as usize).max(1));
    let mut k = 0;
    while k < t {
        let ks: Vec<usize> = if set.local { vec![k] } else { (k..t.min(k + batch)).collect() };
        k += ks.len();
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        progress(0.05 + 0.55 * (k - ks.len()) as f64 / t as f64, &format!("matching {}", labels[ks[0]]));
        let mut read: Vec<(usize, Img, f64)> = vec![];
        for &d in &ks {
            if Some(d) == ref_index {
                rows.push(DateRow {
                    label: labels[d].clone(),
                    x_shift: 0.0,
                    y_shift: 0.0,
                    success: true,
                    reason: "the reference".into(),
                    valid_share: 1.0,
                    is_reference: true,
                    ..Default::default()
                });
                continue;
            }
            let (img, share) = read_date(&mut inputs, &role, d, window, set.mask_buffer)?;
            if share < MIN_CLEAR {
                rows.push(unmatched(&labels[d], clouded(share), share));
                continue;
            }
            read.push((d, img, share));
        }
        if set.local {
            for (d, img, share) in &read {
                let tps = coreg_local::tie_points(
                    &reference.image(w, h),
                    &img.image(w, h),
                    &points,
                    (0.0, 0.0),
                    &geo,
                    &local_params,
                    n_jobs,
                );
                let used: Vec<&TiePoint> = tps.iter().filter(|t| t.outcome.success && !t.outlier).collect();
                let matched = tps.iter().filter(|t| t.outcome.success).count();
                let applied = used.len() >= set.min_points;
                let why = if applied {
                    String::new()
                } else if tps.is_empty() {
                    "no tie point on data of both images".to_string()
                } else if matched == 0 {
                    "no tie point matched".to_string()
                } else {
                    format!("only {} valid tie point(s), fewer than min_points={}", used.len(), set.min_points)
                };
                let mean = |f: &dyn Fn(&TiePoint) -> f64| {
                    if used.is_empty() { f64::NAN } else { used.iter().map(|t| f(t)).sum::<f64>() / used.len() as f64 }
                };
                rows.push(DateRow {
                    label: labels[*d].clone(),
                    x_shift: mean(&|t| t.x_shift_px),
                    y_shift: mean(&|t| t.y_shift_px),
                    reliability: mean(&|t| t.outcome.reliability),
                    ssim_before: mean(&|t| t.outcome.ssim_before),
                    ssim_after: mean(&|t| t.outcome.ssim_after),
                    success: applied,
                    reason: why,
                    applied,
                    counts: Some([tps.len(), matched, used.len()]),
                    valid_share: *share,
                    ..Default::default()
                });
                let at: Vec<(f64, f64)> = tps.iter().map(|t| points[t.index]).collect();
                for (t, &(c, r)) in tps.iter().zip(&at) {
                    let o = &t.outcome;
                    let is_used = o.success && !t.outlier;
                    let nm = planes::csv_num;
                    point_rows.push(vec![
                        labels[*d].clone(),
                        t.index.to_string(),
                        nm(c),
                        nm(r),
                        nm(t.x_map),
                        nm(t.y_map),
                        nm(t.x_shift_px),
                        nm(t.y_shift_px),
                        nm(t.x_shift_map),
                        nm(t.y_shift_map),
                        nm(t.abs_shift),
                        nm(t.angle),
                        nm(o.reliability),
                        nm(o.ssim_before),
                        nm(o.ssim_after),
                        t.ssim_improved.to_string(),
                        o.success.to_string(),
                        o.failure.map_or(String::new(), |f| f.as_str().to_string()),
                        (o.window[2] - o.window[0]).to_string(),
                        (o.window[3] - o.window[1]).to_string(),
                        o.fft_size.to_string(),
                        t.l1.to_string(),
                        t.l2.to_string(),
                        t.l3.to_string(),
                        t.outlier.to_string(),
                        is_used.to_string(),
                        applied.to_string(),
                        nm(*share),
                    ]);
                    if o.success {
                        arrows.push(Arrow {
                            date: labels[*d].clone(),
                            from: [t.x_map, t.y_map],
                            to: [t.x_map + t.x_shift_map * set.arrow_scale, t.y_map + t.y_shift_map * set.arrow_scale],
                            dx_px: t.x_shift_px,
                            dy_px: t.y_shift_px,
                            dx_map: t.x_shift_map,
                            dy_map: t.y_shift_map,
                            reliability: o.reliability,
                            used: is_used,
                            reason: if t.outlier {
                                [(t.l1, "reliability"), (t.l2, "ssim"), (t.l3, "ransac")]
                                    .iter()
                                    .filter(|(f, _)| *f)
                                    .map(|(_, s)| *s)
                                    .collect::<Vec<_>>()
                                    .join("+")
                            } else {
                                String::new()
                            },
                        });
                    }
                }
                if applied {
                    let used_at: Vec<(f64, f64)> =
                        tps.iter().zip(&at).filter(|(t, _)| t.outcome.success && !t.outlier).map(|(_, a)| *a).collect();
                    motions[*d] = Motion::Points(control_points(&used, &used_at));
                }
            }
        } else if !read.is_empty() {
            let joined: Vec<Result<(Outcome, bool), String>> = std::thread::scope(|s| {
                let running: Vec<_> = read
                    .iter()
                    .map(|(_, img, _)| {
                        let (r, cp) = (&reference, &coreg_params);
                        s.spawn(move || match_global(r, img, w, h, cp))
                    })
                    .collect();
                running.into_iter().map(|j| j.join().map_err(|_| "the matching of a date failed".to_string())).collect()
            });
            let outcomes = joined.into_iter().collect::<Result<Vec<(Outcome, bool)>, String>>()?;
            for ((d, _, share), (o, replaced)) in read.iter().zip(outcomes) {
                if replaced {
                    notes.push(format!(
                        "{}: a cloud is near the middle of what the images share, where the window would sit: it sits at the largest clear square, {} x {} pixels.",
                        labels[*d],
                        o.window[2] - o.window[0],
                        o.window[3] - o.window[1]
                    ));
                }
                let (mut ok, mut why) = (o.success, o.failure.map_or(String::new(), |f| f.as_str().to_string()));
                if ok && set.min_reliability > 0.0 && (o.reliability.is_nan() || o.reliability < set.min_reliability) {
                    (ok, why) = (
                        false,
                        format!("reliability {:.1} below min_reliability {}", o.reliability, set.min_reliability),
                    );
                }
                rows.push(DateRow {
                    label: labels[*d].clone(),
                    x_shift: o.x_shift,
                    y_shift: o.y_shift,
                    reliability: o.reliability,
                    ssim_before: o.ssim_before,
                    ssim_after: o.ssim_after,
                    success: ok,
                    reason: why.clone(),
                    window: Some(o.window),
                    fft_size: o.fft_size,
                    applied: ok,
                    counts: None,
                    valid_share: *share,
                    is_reference: false,
                });
                if o.success {
                    let (cx, cy) = ((o.window[0] + o.window[2]) as f64 / 2.0, (o.window[1] + o.window[3]) as f64 / 2.0);
                    let from = [grid.gt[0] + cx * grid.gt[1], grid.gt[3] + cy * grid.gt[5]];
                    let (dx_map, dy_map) = (o.x_shift * grid.gt[1], o.y_shift * grid.gt[5]);
                    arrows.push(Arrow {
                        date: labels[*d].clone(),
                        from,
                        to: [from[0] + dx_map * set.arrow_scale, from[1] + dy_map * set.arrow_scale],
                        dx_px: o.x_shift,
                        dy_px: o.y_shift,
                        dx_map,
                        dy_map,
                        reliability: o.reliability,
                        used: ok,
                        reason: if ok { String::new() } else { why },
                    });
                }
                if ok {
                    motions[*d] = Motion::Shift(o.x_shift, o.y_shift);
                }
            }
        }
    }
    // the rows came in the order the dates were matched: the series' order
    rows.sort_by_key(|r| labels.iter().position(|l| *l == r.label).unwrap_or(usize::MAX));
    drop(reference);

    // ---- the corrected series
    let applied = motions.iter().filter(|m| !matches!(m, Motion::Stay)).count();
    let format = match p.str("format") {
        "none" => None,
        "vrt" => Some(false),
        "cog" => Some(true),
        _ => Some(n > COG_ABOVE),
    };
    let series_dir = dir.join("series");
    let mut series_written = None;
    if let Some(cog) = format {
        let parts = dir.join("parts");
        let sources = dir.join("sources");
        for d in [&series_dir, &parts, &sources] {
            std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
        }
        // the series' own VRTs, kept beside the run so the files of the dates do not depend on the work folder
        let keep = |path: &str, name: &str| -> String {
            let from = Path::new(path);
            if from.extension().is_some_and(|e| e.eq_ignore_ascii_case("vrt")) {
                let to = sources.join(format!("{name}.vrt"));
                if std::fs::copy(from, &to).is_ok() {
                    return to.to_string_lossy().into_owned();
                }
            }
            path.to_string()
        };
        let band_files: Vec<String> = bands.roles.iter().map(|r| keep(&spec.bands[r], r)).collect();
        let qa_file = spec.qa_input.as_deref().map(|q| keep(q, "qa"));
        let has_qa = qa_file.is_some() && inputs.qa_rule().is_some();
        let writer = Writer {
            window,
            grid: &grid,
            band_files,
            qa_file: qa_file.filter(|_| has_qa),
            names: bands.names.clone(),
            qa_name: spec.band_names.get("qa").cloned().unwrap_or_else(|| "quality".into()),
            qa_rule: spec.qa_rule.clone(),
            parts,
            series: series_dir.clone(),
            resampling: set.resampling.clone(),
            order: set.warp.strip_prefix("poly").and_then(|o| o.parse().ok()),
            cog,
            threads,
        };
        let file_names = planes::date_file_names(spec);
        for (d, stem) in file_names.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            progress(0.6 + 0.4 * d as f64 / t as f64, &format!("writing {}", labels[d]));
            writer.date(d, stem, &motions[d], cancel)?;
        }
        if cog {
            // the COGs hold everything: the VRTs they were made from are not needed
            let _ = std::fs::remove_dir_all(&writer.parts);
            let _ = std::fs::remove_dir_all(&sources);
        }
        series_written = Some((has_qa, nb));
    }

    // ---- the tables, the arrows and the report
    let rows_csv = shift_rows(&rows, cell, x0, y0);
    planes::write_csv(&dir.join("shifts.csv"), &SHIFT_HEADER, &rows_csv)?;
    if set.local {
        planes::write_csv(&dir.join("tie_points.csv"), &POINT_HEADER, &point_rows)?;
    }
    let arrows_file = dir.join("shifts.gpkg");
    let has_arrows = !arrows.is_empty();
    if has_arrows {
        write_arrows(&arrows_file, &grid.wkt, &arrows)?;
    }
    let failed: Vec<&DateRow> = rows.iter().filter(|r| !r.applied && !r.is_reference).collect();
    let mut report = format!(
        "Co-registration: {} shift, {} dates, window {w} x {h} pixels\n\nReference: {ref_text}\nBand matched: {role_name}\nMatching window: {} pixels, largest shift {} pixels, margin around the quality band's refusals {} pixels\n",
        if set.local { "local (a grid of tie points a date, COREG_LOCAL)" } else { "global (one shift a date, COREG)" },
        t,
        set.window_size,
        set.max_shift,
        set.mask_buffer
    );
    if set.local {
        report += &format!(
            "Tie points: every {} pixels, filters: level {} (reliability {}, SSIM, RANSAC {}% +- {}%), warp: {}\n",
            set.grid_res,
            set.filter_level,
            local_params.min_reliability,
            set.rs_max_outlier,
            set.rs_tolerance,
            set.warp
        );
    }
    report += &format!("\n{applied} of {t} dates moved to the reference.\n");
    for r in &rows {
        let what = if r.is_reference {
            "the reference".to_string()
        } else if r.applied {
            format!(
                "dx {:+.3}, dy {:+.3} pixels, reliability {:.1}{}",
                r.x_shift,
                r.y_shift,
                r.reliability,
                r.counts.map_or(String::new(), |c| format!(", {} of {} tie points used", c[2], c[0]))
            )
        } else {
            format!("left as it is: {}", r.reason)
        };
        report += &format!("  {}: {what}\n", r.label);
    }
    if !failed.is_empty() {
        report += &format!("\n{} date(s) were not moved (see the reason of each in shifts.csv).\n", failed.len());
    }
    let near = rows.iter().filter(|r| r.applied && r.x_shift.hypot(r.y_shift) > 0.9 * set.max_shift).count();
    if near > 0 {
        report += &format!(
            "\n{near} shift(s) are near the largest one allowed ({} pixels): raise it if the dates are further apart.\n",
            set.max_shift
        );
    }
    let low = rows.iter().filter(|r| r.applied && r.reliability < 60.0).count();
    if low > 0 {
        report += &format!("{low} shift(s) have a reliability under 60: look at them before trusting the series.\n");
    }
    for note in &notes {
        report += &format!("\n{note}\n");
    }
    report += "\nShifts are in pixels of the series: x to the right and y down, the amount a date was moved to fit the reference. The quality band moves by nearest neighbour; pixels brought in from outside the date have no value (the quality band's refused code).\n";
    report += "AROSICS (Scheffler et al. 2017, Remote Sensing 9(7):676, Apache-2.0) as ported in Zeit.\n";
    std::fs::write(dir.join("report.txt"), report).map_err(|e| e.to_string())?;
    let record = json!({
        "mode": p.str("mode"), "reference": ref_text, "band": role_name, "window_size": set.window_size,
        "max_shift": set.max_shift, "mask_buffer": set.mask_buffer, "resampling": set.resampling,
        "dates": t, "moved": applied, "format": p.str("format"),
    });
    std::fs::write(dir.join("coregister.json"), serde_json::to_string_pretty(&record).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;

    let mut outputs = vec![];
    if let Some((has_qa, nb)) = series_written {
        let mut output = json!({
            "id": "aligned", "name": "Co-registered", "unit": "as the series",
            "series": true, "path": series_dir.to_string_lossy(),
        });
        if has_qa {
            output["qa_band"] = json!(nb + 1);
            output["qa_rule"] = json!(spec.qa_rule);
        }
        outputs.push(output);
    }
    if has_arrows {
        outputs.push(json!({
            "id": "shifts", "name": format!("Shifts (x{})", set.arrow_scale), "vector": true,
            "path": arrows_file.to_string_lossy(),
        }));
    }
    progress(1.0, "done");
    Ok(JobResult { outputs, window })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gdal::raster::Buffer;
    use gdal::spatial_ref::SpatialRef;

    #[test]
    fn a_cloud_is_grown_by_a_square_margin() {
        let (w, h) = (11, 9);
        let mut valid = vec![true; w * h];
        valid[4 * w + 5] = false;
        grow_invalid(&mut valid, w, h, 2);
        for (i, &v) in valid.iter().enumerate() {
            let (x, y) = ((i % w) as i64, (i / w) as i64);
            assert_eq!(v, !((x - 5).abs() <= 2 && (y - 4).abs() <= 2), "({x}, {y})");
        }
        // at the edge, and no margin
        let mut valid = vec![true; w * h];
        valid[0] = false;
        let before = valid.clone();
        grow_invalid(&mut valid, w, h, 0);
        assert_eq!(valid, before);
        grow_invalid(&mut valid, w, h, 1);
        assert_eq!(valid.iter().filter(|&&v| !v).count(), 4);
    }

    #[test]
    fn the_grid_of_tie_points_is_aroscis_arange() {
        // np.arange(0, size + res, res): the corners of the cells and one past the last
        assert_eq!(grid_points(500, 200), [0.0, 200.0, 400.0, 600.0]);
        assert_eq!(grid_points(400, 200), [0.0, 200.0, 400.0]);
        assert_eq!(grid_points(10, 200), [0.0, 200.0]);
    }

    #[test]
    fn the_largest_clear_square_is_found() {
        let (w, h) = (20, 12);
        let mut a = vec![true; w * h];
        // a hole in the middle of the top: the largest square is under it or beside it
        for y in 0..6 {
            for x in 8..11 {
                a[y * w + x] = false;
            }
        }
        let b = vec![true; w * h];
        let (cx, cy, side) = largest_valid_square(&a, &b, w, h).unwrap();
        // 9 columns on the right of the hole (11-19) by 12 rows: a square of 9; 8 on its left; 6 rows under the hole
        assert_eq!(side, 9);
        assert!((cx - 15.5).abs() < 1e-9 && (4.5..=7.5).contains(&cy), "{cx} {cy}");
        assert!(largest_valid_square(&[false; 4], &[true; 4], 2, 2).is_none());
    }

    #[test]
    fn at_most_7000_control_points_spread_evenly() {
        let all: Vec<[f64; 4]> = (0..20000).map(|i| [i as f64, 0.0, 0.0, 0.0]).collect();
        let kept = spread(all);
        assert_eq!(kept.len(), MAX_GCPS);
        assert_eq!((kept[0][0], kept[MAX_GCPS - 1][0]), (0.0, 19999.0));
        assert!(kept.windows(2).all(|p| p[1][0] > p[0][0]));
        let few: Vec<[f64; 4]> = (0..10).map(|i| [i as f64, 0.0, 0.0, 0.0]).collect();
        assert_eq!(spread(few).len(), 10);
    }

    /// A 64 x 64 band whose value is its column + 100 its row, and the quality band's codes, as files.
    fn sources(dir: &Path) -> (String, String, Grid) {
        let (w, h) = (64usize, 64usize);
        let wkt = SpatialRef::from_epsg(32723).unwrap().to_wkt().unwrap();
        let gt = [600000.0, 30.0, 0.0, 7500000.0, 0.0, -30.0];
        let make = |name: &str, f: &dyn Fn(usize, usize) -> f32| -> String {
            let path = dir.join(name);
            let drv = DriverManager::get_driver_by_name("GTiff").unwrap();
            let mut ds = drv.create_with_band_type::<f32, _>(&path, w, h, 1).unwrap();
            ds.set_geo_transform(&gt).unwrap();
            ds.set_projection(&wkt).unwrap();
            let v: Vec<f32> = (0..w * h).map(|i| f(i % w, i / w)).collect();
            ds.rasterband(1).unwrap().write((0, 0), (w, h), &mut Buffer::new((w, h), v)).unwrap();
            path.to_string_lossy().into_owned()
        };
        let band = make("band.tif", &|x, y| x as f32 + 100.0 * y as f32);
        let qa = make("qa.tif", &|x, _| if x < 8 { 4.0 } else { 0.0 });
        (band, qa, Grid { wkt, gt, w, h })
    }

    fn writer<'a>(dir: &Path, grid: &'a Grid, band: &str, qa: &str, order: Option<u8>) -> Writer<'a> {
        let (parts, series) = (dir.join("parts"), dir.join("series"));
        std::fs::create_dir_all(&parts).unwrap();
        std::fs::create_dir_all(&series).unwrap();
        Writer {
            window: [0, 0, grid.w, grid.h],
            grid,
            band_files: vec![band.to_string()],
            qa_file: Some(qa.to_string()),
            names: vec!["value".into()],
            qa_name: "Fmask".into(),
            qa_rule: Some("fmask".into()),
            parts,
            series,
            resampling: "near".into(),
            order,
            cog: false,
            threads: 1,
        }
    }

    fn read_band(path: &Path, b: usize) -> Vec<f32> {
        let ds = Dataset::open(path).unwrap();
        let (w, h) = ds.raster_size();
        ds.rasterband(b).unwrap().read_as::<f32>((0, 0), (w, h), (w, h), None).unwrap().data().to_vec()
    }

    /// A shift moves the content by (x, y) cells (the new cell (r, c) is the old (r - y, c - x)), the cells that come in
    /// from outside have no value and the quality band is moved by nearest neighbour, its new cells refused (255).
    #[test]
    fn a_shift_moves_the_content_and_the_quality_band_by_whole_cells() {
        let dir = std::env::temp_dir().join(format!("janus-coregister-unit-shift-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (band, qa, grid) = sources(&dir);
        let wr = writer(&dir, &grid, &band, &qa, None);
        let none = AtomicBool::new(false);
        let out = wr.date(0, "d", &Motion::Shift(3.0, -2.0), &none).unwrap();
        let ds = Dataset::open(&out).unwrap();
        assert_eq!(ds.raster_count(), 2);
        assert_eq!(ds.geo_transform().unwrap(), grid.gt);
        let v = read_band(&out, 1);
        let w = grid.w;
        // the new cell (r, c) is the old (r + 2, c - 3)
        assert_eq!(v[10 * w + 20], 17.0 + 100.0 * 12.0);
        assert!(v[10 * w + 1].is_nan(), "from outside on the left");
        assert!(v[(grid.h - 1) * w + 20].is_nan(), "from outside below");
        // the codes: the cloud's columns 0-7 now 3-10, the new columns 0-2 refused
        let q = read_band(&out, 2);
        assert!(q.iter().all(|c| [0.0, 4.0, 255.0].contains(c)));
        assert_eq!((q[10 * w + 1], q[10 * w + 5], q[10 * w + 12]), (255.0, 4.0, 0.0));
        // a date that stays: the source's cells, read through the VRT
        let stay = wr.date(0, "s", &Motion::Stay, &none).unwrap();
        assert_eq!(read_band(&stay, 1)[10 * w + 20], 20.0 + 1000.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Ground control points that say the same as a shift (each pixel goes to the cell 3 columns on and 2 rows up) warp
    /// the same, as a polynomial of order 1 and as a thin plate spline.
    #[test]
    fn control_points_that_say_a_shift_warp_as_the_shift_does() {
        let dir = std::env::temp_dir().join(format!("janus-coregister-unit-gcps-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (band, qa, grid) = sources(&dir);
        let none = AtomicBool::new(false);
        let g = grid.gt;
        let at = [(0.0, 0.0), (64.0, 0.0), (0.0, 64.0), (64.0, 64.0), (32.0, 32.0), (16.0, 48.0), (48.0, 16.0)];
        let points: Vec<[f64; 4]> =
            at.iter().map(|&(c, r)| [c, r, g[0] + (c + 3.0) * g[1], g[3] + (r - 2.0) * g[5]]).collect();
        let wr = writer(&dir, &grid, &band, &qa, Some(1));
        let shifted = wr.date(0, "shift", &Motion::Shift(3.0, -2.0), &none).unwrap();
        let by_poly = wr.date(0, "poly", &Motion::Points(points.clone()), &none).unwrap();
        let by_tps = writer(&dir, &grid, &band, &qa, None).date(0, "tps", &Motion::Points(points), &none).unwrap();
        let w = grid.w;
        let want = read_band(&shifted, 1);
        for (name, file) in [("poly", by_poly), ("tps", by_tps)] {
            let got = read_band(&file, 1);
            for y in 8..grid.h - 8 {
                for x in 8..w - 8 {
                    let (a, b) = (got[y * w + x], want[y * w + x]);
                    assert!((a - b).abs() < 0.05, "{name} ({x}, {y}): {a} against {b}");
                }
            }
            // the codes stay codes
            assert!(read_band(&file, 2).iter().all(|c| [0.0, 4.0, 255.0].contains(c)), "{name}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Under the rule that refuses code 0 the cloud's cells must stay 0 when they are moved (the cells that come in
    /// from outside are 0 too): GDAL's destination NoData would turn them into 1.
    #[test]
    fn a_quality_band_of_zeros_and_ones_keeps_its_zeros_when_moved() {
        let dir = std::env::temp_dir().join(format!("janus-coregister-unit-zeros-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (band, _, grid) = sources(&dir);
        let qa = {
            let path = dir.join("qa01.tif");
            let drv = DriverManager::get_driver_by_name("GTiff").unwrap();
            let mut ds = drv.create_with_band_type::<f32, _>(&path, grid.w, grid.h, 1).unwrap();
            ds.set_geo_transform(&grid.gt).unwrap();
            ds.set_projection(&grid.wkt).unwrap();
            let v: Vec<f32> = (0..grid.w * grid.h).map(|i| if i % grid.w < 8 { 0.0 } else { 1.0 }).collect();
            ds.rasterband(1).unwrap().write((0, 0), (grid.w, grid.h), &mut Buffer::new((grid.w, grid.h), v)).unwrap();
            path.to_string_lossy().into_owned()
        };
        let mut wr = writer(&dir, &grid, &band, &qa, None);
        wr.qa_rule = Some("nonzero".into());
        let out = wr.date(0, "z", &Motion::Shift(3.0, -2.0), &AtomicBool::new(false)).unwrap();
        let q = read_band(&out, 2);
        let w = grid.w;
        // columns 0-2 came from outside (refused: 0), the cloud's columns 0-7 are now 3-10, the rest clear
        assert_eq!((q[10 * w + 1], q[10 * w + 5], q[10 * w + 12]), (0.0, 0.0, 1.0));
        assert!(q.iter().all(|c| *c == 0.0 || *c == 1.0));
        // the 11 columns of the rows that have a source, and the two rows that came in from below
        assert_eq!(q.iter().filter(|c| **c == 0.0).count(), 11 * (grid.h - 2) + 2 * grid.w);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_quality_code_a_warp_brings_in_is_one_the_rule_refuses() {
        assert_eq!(fill_code(Some("fmask")), 255.0);
        assert_eq!(fill_code(Some("hls_fmask")), 255.0);
        assert_eq!(fill_code(Some("landsat_c2")), 1.0);
        for rule in ["fmask", "hls_fmask", "landsat_c2", "nonzero", "scl"] {
            assert!(!crate::job::qa_usable(Some(rule), fill_code(Some(rule))), "{rule}");
        }
    }
}
