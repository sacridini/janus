//! Build a series and save the series (IDEIAS phase 46). Building: loose files grouped by the date in their names
//! (as when opening), a date's single-band files stacked as its bands (GDALBuildVRT -separate) and its tiles (files
//! of other extents) mosaicked, every date on one grid (the files' extent, their finest pixel); a folder of VRTs
//! (at once) or of COGs, `<name>_<YYYYMMDD>`. Saving: the active series (the quantity shown, its mask applied, or
//! every band) over a window into a multiband COG (a band per date, its date in the band's description: it opens
//! back as a series) or a NetCDF (CF) / Zarr with a time axis through GDAL's multidimensional API.

use std::collections::BTreeMap;
use std::ffi::{CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use gdal::spatial_ref::SpatialRef;
use gdal::{Dataset, Metadata};
use janus_cube::CubeInfo;
use janus_cube::dates::{civil_from_days, parse_date};

use crate::polygonize::last_error;
use crate::regularize::{read_date, to_cogs};
use crate::warp::Grid;

/// A date of the series to build: its files.
#[derive(Clone, Debug)]
pub struct PlanDate {
    /// Unix seconds
    pub time: f64,
    /// YYYY-MM-DD
    pub label: String,
    /// each band's files (one, or the tiles mosaicked); files of several bands: one entry, the tiles
    pub bands: Vec<Vec<String>>,
}

/// What the files make: the dates, the bands' names, the grid, the files left out.
#[derive(Clone, Debug, Default)]
pub struct BuildPlan {
    pub dates: Vec<PlanDate>,
    pub bands: Vec<String>,
    /// files of several bands each: a date's tiles mosaicked, their bands kept
    pub multiband: bool,
    pub grid: Option<Grid>,
    /// (file, why)
    pub skipped: Vec<(String, String)>,
}

/// A file read for the plan.
struct Input {
    path: String,
    time: f64,
    key: String,
    ext: [f64; 4],
    res: (f64, f64),
    bands: usize,
    descs: Vec<String>,
}

/// The day's text YYYY-MM-DD of Unix seconds.
pub fn day_text(secs: f64) -> String {
    let (y, m, d) = civil_from_days((secs / 86400.0).floor() as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

/// A file's stem without its dates (runs of 6 digits or more): what tells its band apart within a date.
fn key_of(stem: &str) -> String {
    let mut out = String::new();
    let mut run = String::new();
    for ch in stem.chars().map(Some).chain([None]) {
        if let Some(d) = ch.filter(char::is_ascii_digit) {
            run.push(d);
            continue;
        }
        if run.len() < 6 {
            out += &run;
        }
        run.clear();
        out.extend(ch);
    }
    out
}

/// The names that tell keys apart: their common beginning and end cut off.
fn band_names(keys: &[String]) -> Vec<String> {
    if keys.len() < 2 {
        return keys.iter().map(|k| if k.is_empty() { "band 1".into() } else { k.clone() }).collect();
    }
    let chars: Vec<Vec<char>> = keys.iter().map(|k| k.chars().collect()).collect();
    let short = chars.iter().map(Vec::len).min().unwrap_or(0);
    let pre = (0..short).take_while(|&i| chars.iter().all(|c| c[i] == chars[0][i])).count();
    let suf = (0..short - pre)
        .take_while(|&i| chars.iter().all(|c| c[c.len() - 1 - i] == chars[0][chars[0].len() - 1 - i]))
        .count();
    // cut at a separator: B04 and B08 stay themselves (not 4 and 8)
    let sep = |ch: char| "_-. ".contains(ch);
    let pre = (0..=pre).rev().find(|&i| i == 0 || sep(chars[0][i - 1])).unwrap_or(0);
    let suf = (0..=suf).rev().find(|&j| j == 0 || sep(chars[0][chars[0].len() - j])).unwrap_or(0);
    chars
        .iter()
        .enumerate()
        .map(|(b, c)| {
            let n: String = c[pre..c.len() - suf].iter().collect();
            let n = n.trim_matches(|ch: char| "_-. ".contains(ch)).to_string();
            if n.is_empty() { format!("band {}", b + 1) } else { n }
        })
        .collect()
}

/// Reads the files and plans the series: dates from the names, bands and tiles, one CRS, the grid.
pub fn plan(files: &[String]) -> Result<BuildPlan, String> {
    let mut p = BuildPlan::default();
    let mut srs0: Option<(SpatialRef, String)> = None;
    let mut inputs: Vec<Input> = vec![];
    for path in files {
        let name = Path::new(path).file_name().map_or(path.clone(), |n| n.to_string_lossy().into_owned());
        let Some((time, _)) = parse_date(&name) else {
            p.skipped.push((path.clone(), "no date in its name".into()));
            continue;
        };
        let ds = match Dataset::open(path) {
            Ok(d) => d,
            Err(e) => {
                p.skipped.push((path.clone(), e.to_string()));
                continue;
            }
        };
        let Ok(gt) = ds.geo_transform() else {
            p.skipped.push((path.clone(), "no georeference".into()));
            continue;
        };
        if gt[2] != 0.0 || gt[4] != 0.0 {
            p.skipped.push((path.clone(), "a rotated grid".into()));
            continue;
        }
        let wkt = ds.projection();
        let same = match (&srs0, SpatialRef::from_wkt(&wkt)) {
            (None, Ok(s)) => {
                srs0 = Some((s, wkt.clone()));
                true
            }
            (Some((s0, _)), Ok(s)) => *s0 == s,
            _ => false,
        };
        if !same {
            p.skipped.push((
                path.clone(),
                "another CRS than the first file's: reproject it first (Tools > Reproject, resample, clip)".into(),
            ));
            continue;
        }
        let (w, h) = ds.raster_size();
        let (x1, y0) = (gt[0] + w as f64 * gt[1], gt[3] + h as f64 * gt[5]);
        let stem = Path::new(path).file_stem().map_or(String::new(), |s| s.to_string_lossy().into_owned());
        let n = ds.raster_count();
        inputs.push(Input {
            path: path.clone(),
            time,
            key: key_of(&stem),
            ext: [gt[0].min(x1), y0.min(gt[3]), gt[0].max(x1), y0.max(gt[3])],
            res: (gt[1].abs(), gt[5].abs()),
            bands: n,
            descs: (1..=n)
                .map(|b| ds.rasterband(b).ok().and_then(|r| r.description().ok()).unwrap_or_default())
                .collect(),
        });
    }
    // the dates, each with its signature: (several bands a file, number of bands)
    let mut by_date: BTreeMap<i64, Vec<&Input>> = BTreeMap::new();
    for i in &inputs {
        by_date.entry(i.time.round() as i64).or_default().push(i);
    }
    type Planned<'a> = (PlanDate, (bool, usize), Vec<&'a Input>);
    let mut planned: Vec<Planned> = vec![];
    for (t, files) in by_date {
        let multi = files[0].bands > 1;
        let skip = |p: &mut BuildPlan, why: &str| {
            for f in &files {
                p.skipped.push((f.path.clone(), why.to_string()));
            }
        };
        if files.iter().any(|f| (f.bands > 1) != multi) {
            skip(&mut p, "its date mixes files of one band and of several");
            continue;
        }
        if multi {
            if files.iter().any(|f| f.bands != files[0].bands) {
                skip(&mut p, "its date's files have different numbers of bands");
                continue;
            }
            let date = PlanDate {
                time: t as f64,
                label: day_text(t as f64),
                bands: vec![files.iter().map(|f| f.path.clone()).collect()],
            };
            planned.push((date, (true, files[0].bands), files));
            continue;
        }
        // tiles: the files of one extent; within a tile, its bands by name
        let mut tiles: Vec<Vec<&Input>> = vec![];
        for f in &files {
            let near = |a: &Input| (0..4).all(|k| (a.ext[k] - f.ext[k]).abs() < 0.5 * f.res.0.min(f.res.1));
            match tiles.iter_mut().find(|t| near(t[0])) {
                Some(t) => t.push(f),
                None => tiles.push(vec![f]),
            }
        }
        for t in &mut tiles {
            t.sort_by(|a, b| janus_cube::inputs::natural_cmp(&a.key, &b.key));
        }
        let n = tiles[0].len();
        if tiles.iter().any(|t| t.len() != n) {
            skip(&mut p, "its tiles have different numbers of files (bands)");
            continue;
        }
        let date = PlanDate {
            time: t as f64,
            label: day_text(t as f64),
            bands: (0..n).map(|b| tiles.iter().map(|tile| tile[b].path.clone()).collect()).collect(),
        };
        let first: Vec<&Input> = tiles[0].clone();
        planned.push((date, (false, n), first));
    }
    // the most common signature; the dates of another left out
    let mut counts: BTreeMap<(bool, usize), usize> = BTreeMap::new();
    for d in &planned {
        *counts.entry(d.1).or_default() += 1;
    }
    let Some((&sig, _)) = counts.iter().max_by_key(|(k, c)| (**c, k.1)) else {
        return Ok(p);
    };
    p.multiband = sig.0;
    let mut keys: Vec<String> = vec![];
    for (date, s, first) in planned {
        if s != sig {
            for f in date.bands.iter().flatten() {
                p.skipped.push((f.clone(), format!("{} band(s) at its date where the others have {}", s.1, sig.1)));
            }
            continue;
        }
        if p.bands.is_empty() {
            p.bands = if sig.0 {
                first[0]
                    .descs
                    .iter()
                    .enumerate()
                    .map(|(b, d)| if d.trim().is_empty() { format!("band {}", b + 1) } else { d.trim().to_string() })
                    .collect()
            } else {
                band_names(&first.iter().map(|f| f.key.clone()).collect::<Vec<_>>())
            };
        }
        keys.push(first[0].key.clone());
        p.dates.push(date);
    }
    // one band of a file a date: what every date's name has in common (lc_2000, lc_2001: lc)
    if !p.multiband && p.bands.len() == 1 && keys.len() > 1 {
        let chars: Vec<Vec<char>> = keys.iter().map(|k| k.chars().collect()).collect();
        let short = chars.iter().map(Vec::len).min().unwrap_or(0);
        let pre: String =
            (0..short).take_while(|&i| chars.iter().all(|c| c[i] == chars[0][i])).map(|i| chars[0][i]).collect();
        let pre = pre.trim_matches(|ch: char| "_-. ".contains(ch) || ch.is_ascii_digit()).to_string();
        p.bands = vec![if pre.is_empty() { "band 1".into() } else { pre }];
    }
    // the grid: the files' extent at their finest pixel
    let used: std::collections::HashSet<&String> = p.dates.iter().flat_map(|d| d.bands.iter().flatten()).collect();
    let (mut e, mut rx, mut ry) = ([f64::MAX, f64::MAX, f64::MIN, f64::MIN], f64::MAX, f64::MAX);
    for i in inputs.iter().filter(|i| used.contains(&i.path)) {
        e = [e[0].min(i.ext[0]), e[1].min(i.ext[1]), e[2].max(i.ext[2]), e[3].max(i.ext[3])];
        (rx, ry) = (rx.min(i.res.0), ry.min(i.res.1));
    }
    if let Some((_, wkt)) = srs0
        && !p.dates.is_empty()
    {
        let (w, h) = (((e[2] - e[0]) / rx).round() as usize, ((e[3] - e[1]) / ry).round() as usize);
        p.grid = Some(Grid { wkt, gt: [e[0], rx, 0.0, e[3], 0.0, -ry], w, h });
    }
    Ok(p)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BuildFormat {
    /// VRTs: nothing written but where the pixels are, ready at once
    #[default]
    Vrt,
    Cog,
}

/// NULL-terminated C strings for GDAL's argument lists.
struct Args {
    _owned: Vec<CString>,
    ptrs: Vec<*mut c_char>,
}

impl Args {
    fn new(args: &[String]) -> Args {
        let owned: Vec<CString> = args.iter().map(|a| CString::new(a.as_str()).unwrap_or_default()).collect();
        let mut ptrs: Vec<*mut c_char> = owned.iter().map(|c| c.as_ptr() as *mut c_char).collect();
        ptrs.push(null_mut());
        Args { _owned: owned, ptrs }
    }
}

/// gdalbuildvrt's own code: `srcs` into the VRT `dst` with `args`.
fn build_vrt(dst: &Path, srcs: &[String], args: &[String]) -> Result<(), String> {
    let a = Args::new(args);
    let s = Args::new(srcs);
    let d = CString::new(dst.to_string_lossy().as_ref()).unwrap_or_default();
    // SAFETY: the C strings outlive the call; the options and the dataset are freed here.
    unsafe {
        gdal_sys::CPLErrorReset();
        let opts = gdal_sys::GDALBuildVRTOptionsNew(a.ptrs.as_ptr() as *mut *mut c_char, null_mut());
        if opts.is_null() {
            return Err(format!("gdalbuildvrt {}: {}", args.join(" "), last_error()));
        }
        let mut usage = 0;
        let ds = gdal_sys::GDALBuildVRT(
            d.as_ptr(),
            srcs.len() as i32,
            null_mut(),
            s.ptrs.as_ptr() as *const *const c_char,
            opts,
            &mut usage,
        );
        gdal_sys::GDALBuildVRTOptionsFree(opts);
        if ds.is_null() {
            return Err(format!("{}: {}", dst.display(), last_error()));
        }
        gdal_sys::GDALClose(ds);
    }
    Ok(())
}

/// Builds the planned series in `out_dir` as `<stem>_<YYYYMMDD>.vrt` (or .tif); returns the files.
pub fn build(
    plan: &BuildPlan,
    out_dir: &Path,
    stem: &str,
    format: BuildFormat,
    threads: usize,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<Vec<String>, String> {
    let g = plan.grid.as_ref().ok_or("no date to build")?;
    std::fs::create_dir_all(out_dir).map_err(|e| format!("{}: {e}", out_dir.display()))?;
    // the dates' VRTs: in the folder, or beside it for the COGs; the bands' mosaics in .bands
    let vrts = if format == BuildFormat::Vrt { out_dir.to_path_buf() } else { out_dir.join(".part") };
    let bands_dir = out_dir.join(".bands");
    let grid_args = {
        let (x0, y1) = (g.gt[0], g.gt[3]);
        let (x1, y0) = (x0 + g.w as f64 * g.gt[1], y1 + g.h as f64 * g.gt[5]);
        [x0, y0, x1, y1].iter().map(|v| format!("{v}")).collect::<Vec<_>>()
    };
    let common: Vec<String> = ["-te"]
        .iter()
        .map(|s| s.to_string())
        .chain(grid_args)
        .chain(["-tr".to_string(), format!("{}", g.gt[1]), format!("{}", -g.gt[5]), "-r".into(), "nearest".into()])
        .collect();
    std::fs::create_dir_all(&vrts).map_err(|e| format!("{}: {e}", vrts.display()))?;
    let mut made: Vec<PathBuf> = vec![];
    for (k, d) in plan.dates.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        progress(0.9 * k as f64 / plan.dates.len() as f64, &format!("{}: {}", d.label, "building its VRT"));
        let name = format!("{stem}_{}", d.label.replace('-', ""));
        let dst = vrts.join(format!("{name}.vrt"));
        if plan.multiband {
            build_vrt(&dst, &d.bands[0], &common)?;
        } else {
            // each band one file or its tiles' mosaic, then the bands stacked
            let mut srcs = vec![];
            for (b, files) in d.bands.iter().enumerate() {
                if files.len() == 1 {
                    srcs.push(files[0].clone());
                } else {
                    std::fs::create_dir_all(&bands_dir).map_err(|e| format!("{}: {e}", bands_dir.display()))?;
                    let m = bands_dir.join(format!("{name}_b{}.vrt", b + 1));
                    build_vrt(&m, files, &common)?;
                    srcs.push(m.to_string_lossy().into_owned());
                }
            }
            let mut args = vec!["-separate".to_string()];
            args.extend(common.iter().cloned());
            build_vrt(&dst, &srcs, &args)?;
        }
        // the bands' names on the date's bands
        let ds = Dataset::open_ex(
            &dst,
            gdal::DatasetOptions {
                open_flags: gdal::GdalOpenFlags::GDAL_OF_UPDATE | gdal::GdalOpenFlags::GDAL_OF_RASTER,
                ..Default::default()
            },
        )
        .map_err(|e| format!("{}: {e}", dst.display()))?;
        for (b, n) in plan.bands.iter().enumerate() {
            if let Ok(mut rb) = ds.rasterband(b + 1) {
                let _ = rb.set_description(n);
            }
        }
        drop(ds);
        made.push(dst);
    }
    if format == BuildFormat::Vrt {
        progress(1.0, "done");
        return Ok(made.iter().map(|p| p.to_string_lossy().into_owned()).collect());
    }
    let files = to_cogs(&made, out_dir, threads, &|f| progress(0.9 + 0.1 * f, "writing the COGs"), cancel)?;
    let _ = std::fs::remove_dir_all(&vrts);
    let _ = std::fs::remove_dir_all(&bands_dir);
    progress(1.0, "done");
    Ok(files)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SaveFormat {
    /// a band per date, its date in the band's description
    #[default]
    Cog,
    NetCdf,
    Zarr,
}

#[derive(Clone, Debug, Default)]
pub struct SaveOptions {
    pub format: SaveFormat,
    /// (x0, y0, x1, y1) in the series' pixels (None: all of it)
    pub window: Option<[usize; 4]>,
    /// every band of the dates, a variable each (NetCDF and Zarr; else the quantity shown, its mask applied)
    pub all_bands: bool,
    /// the quantity's name (its variable)
    pub name: String,
    pub threads: usize,
}

pub struct Saved {
    pub dates: usize,
    pub variables: Vec<String>,
}

/// A variable's name: letters, digits and _, not starting with a digit.
fn var_name(s: &str) -> String {
    let mut v: String = s.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    v = v.trim_matches('_').to_string();
    if v.is_empty() || v.starts_with(|c: char| c.is_ascii_digit()) {
        v = format!("v_{v}");
    }
    v
}

/// Saves the series (its window) to `out`.
pub fn save_series(
    info: &Arc<CubeInfo>,
    opts: &SaveOptions,
    out: &Path,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<Saved, String> {
    let win = opts.window.unwrap_or([0, 0, info.width, info.height]);
    let (w, h) = (win[2].saturating_sub(win[0]), win[3].saturating_sub(win[1]));
    if w == 0 || h == 0 {
        return Err("an empty window".into());
    }
    let all = opts.all_bands && opts.format != SaveFormat::Cog && info.bands_per_date > 1;
    let bands = if all { info.bands_per_date } else { 1 };
    let names: Vec<String> = if all {
        (0..bands).map(|b| var_name(info.band_names.get(b).map_or(&format!("band_{}", b + 1), |s| s))).collect()
    } else {
        vec![var_name(if opts.name.is_empty() { "value" } else { &opts.name })]
    };
    let t_n = info.t();
    let gt = info.geo_transform.unwrap_or([0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
    let (x0, y0) = (win[0] as f64, win[1] as f64);
    let wgt = [gt[0] + x0 * gt[1] + y0 * gt[2], gt[1], gt[2], gt[3] + x0 * gt[4] + y0 * gt[5], gt[4], gt[5]];
    let labels: Vec<String> =
        info.layers.iter().map(|l| if info.time_is_date { day_text(l.time) } else { l.label.clone() }).collect();
    let _ = std::fs::remove_file(out);
    if opts.format != SaveFormat::Cog && out.exists() {
        let _ = std::fs::remove_dir_all(out);
    }
    let mut sink = match opts.format {
        SaveFormat::Cog => Sink::tiff(out, w, h, t_n, &wgt, &info.crs_wkt, &labels)?,
        f => Sink::multidim(f, out, w, h, &wgt, info, &names, opts.threads)?,
    };
    // rows a block: every date's bands of the block in memory, 256 MB at most
    let rows = ((256usize << 20) / (w * t_n * bands * 4).max(1)).clamp(1, 512).min(h);
    let blocks = h.div_ceil(rows);
    let threads = opts.threads.max(1);
    for (k, by) in (0..h).step_by(rows).enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let r = rows.min(h - by);
        let n = w * r;
        progress(0.9 * k as f64 / blocks as f64, &format!("rows {} to {} of {h}", by + 1, by + r));
        let mut values = vec![f32::NAN; t_n * bands * n];
        let next = AtomicUsize::new(0);
        let chunks: Vec<std::sync::Mutex<&mut [f32]>> =
            values.chunks_mut(bands * n).map(std::sync::Mutex::new).collect();
        std::thread::scope(|s| {
            for _ in 0..threads.min(t_n) {
                s.spawn(|| {
                    loop {
                        let t = next.fetch_add(1, Ordering::Relaxed);
                        if t >= t_n || cancel.load(Ordering::Relaxed) {
                            break;
                        }
                        let mut c = chunks[t].lock().unwrap();
                        if !read_date(info, t, all, (win[0], win[1] + by, w, r), &mut c) {
                            c.fill(f32::NAN);
                        }
                    }
                });
            }
        });
        drop(chunks);
        for t in 0..t_n {
            for b in 0..bands {
                let off = (t * bands + b) * n;
                sink.write(t, b, by, r, &values[off..off + n])?;
            }
        }
    }
    sink.finish(out, opts.threads, &|f| progress(0.9 + 0.1 * f, "writing the COG"), cancel)?;
    progress(1.0, "done");
    Ok(Saved { dates: t_n, variables: names })
}

/// Where the values go: a GeoTIFF of a band per date (made a COG at the end), or a multidimensional dataset.
enum Sink {
    Tiff {
        ds: Dataset,
        temp: PathBuf,
    },
    Multidim {
        ds: gdal_sys::GDALDatasetH,
        arrays: Vec<gdal_sys::GDALMDArrayH>,
        w: usize,
        f32t: gdal_sys::GDALExtendedDataTypeH,
    },
}

impl Sink {
    fn tiff(
        out: &Path,
        w: usize,
        h: usize,
        t: usize,
        gt: &[f64; 6],
        wkt: &str,
        labels: &[String],
    ) -> Result<Sink, String> {
        let temp = out.with_extension("part.tif");
        let drv = gdal::DriverManager::get_driver_by_name("GTiff").map_err(|e| e.to_string())?;
        let co = gdal::raster::RasterCreationOptions::from_iter(["TILED=YES", "BIGTIFF=IF_SAFER", "INTERLEAVE=BAND"]);
        let mut ds = drv
            .create_with_band_type_with_options::<f32, _>(&temp, w, h, t, &co)
            .map_err(|e| format!("{}: {e}", temp.display()))?;
        ds.set_geo_transform(gt).map_err(|e| e.to_string())?;
        if !wkt.is_empty() {
            ds.set_projection(wkt).map_err(|e| e.to_string())?;
        }
        for (b, l) in labels.iter().enumerate() {
            let mut rb = ds.rasterband(b + 1).map_err(|e| e.to_string())?;
            rb.set_no_data_value(Some(f64::NAN)).map_err(|e| e.to_string())?;
            rb.set_description(l).map_err(|e| e.to_string())?;
        }
        Ok(Sink::Tiff { ds, temp })
    }

    #[allow(clippy::too_many_arguments)]
    fn multidim(
        format: SaveFormat,
        out: &Path,
        w: usize,
        h: usize,
        gt: &[f64; 6],
        info: &CubeInfo,
        names: &[String],
        threads: usize,
    ) -> Result<Sink, String> {
        let c = |s: &str| CString::new(s).unwrap_or_default();
        let t_n = info.t();
        // SAFETY: GDAL's multidimensional C API; every handle made here is released here or by finish.
        unsafe {
            gdal_sys::CPLErrorReset();
            let (drv, ds_opts, arr_opts): (&str, Vec<&str>, Vec<String>) = match format {
                SaveFormat::Zarr => (
                    "Zarr",
                    vec!["FORMAT=ZARR_V2"],
                    // Blosc's LZ4 with byte shuffling on the threads, what xarray's to_zarr writes
                    vec![
                        "COMPRESS=BLOSC".into(),
                        "BLOSC_CNAME=lz4".into(),
                        "BLOSC_CLEVEL=5".into(),
                        "BLOSC_SHUFFLE=BYTE".into(),
                        format!("BLOSC_NUM_THREADS={}", threads.max(1)),
                        format!("BLOCKSIZE=1,{},{}", h.min(512), w.min(512)),
                    ],
                ),
                _ => (
                    "netCDF",
                    vec!["FORMAT=NC4"],
                    vec!["COMPRESS=DEFLATE".into(), format!("BLOCKSIZE=1,{},{}", h.min(512), w.min(512))],
                ),
            };
            let d = gdal_sys::GDALGetDriverByName(c(drv).as_ptr());
            if d.is_null() {
                return Err(format!("no {drv} driver in this GDAL"));
            }
            let mut dso = null_mut();
            for o in &ds_opts {
                dso = gdal_sys::CSLAddString(dso, c(o).as_ptr());
            }
            let ds = gdal_sys::GDALCreateMultiDimensional(d, c(&out.to_string_lossy()).as_ptr(), null_mut(), dso);
            gdal_sys::CSLDestroy(dso);
            if ds.is_null() {
                return Err(format!("{}: {}", out.display(), last_error()));
            }
            let root = gdal_sys::GDALDatasetGetRootGroup(ds);
            let geographic = SpatialRef::from_wkt(&info.crs_wkt).is_ok_and(|s| s.is_geographic());
            let f64t = gdal_sys::GDALExtendedDataTypeCreate(gdal_sys::GDALDataType::GDT_Float64);
            let f32t = gdal_sys::GDALExtendedDataTypeCreate(gdal_sys::GDALDataType::GDT_Float32);
            let st = gdal_sys::GDALExtendedDataTypeCreateString(0);
            let attr = |arr: gdal_sys::GDALMDArrayH, k: &str, v: &str| {
                let a = gdal_sys::GDALMDArrayCreateAttribute(arr, c(k).as_ptr(), 0, null(), st, null_mut());
                if !a.is_null() {
                    gdal_sys::GDALAttributeWriteString(a, c(v).as_ptr());
                    gdal_sys::GDALAttributeRelease(a);
                }
            };
            // the dimensions and their coordinates (named as the dimensions: their indexing variables): time (days
            // since 1970), y and x (the pixels' centres)
            let (yn, xn) = if geographic { ("lat", "lon") } else { ("y", "x") };
            let dims = [("time", "TEMPORAL", t_n), (yn, "HORIZONTAL_Y", h), (xn, "HORIZONTAL_X", w)];
            let mut hd: Vec<gdal_sys::GDALDimensionH> = vec![];
            for (n, ty, size) in dims {
                let dim = gdal_sys::GDALGroupCreateDimension(
                    root,
                    c(n).as_ptr(),
                    c(ty).as_ptr(),
                    null(),
                    size as u64,
                    null_mut(),
                );
                if dim.is_null() {
                    return Err(format!("the dimension {n}: {}", last_error()));
                }
                hd.push(dim);
            }
            let coords: [Vec<f64>; 3] = [
                info.layers.iter().map(|l| if info.time_is_date { l.time / 86400.0 } else { l.time }).collect(),
                (0..h).map(|j| gt[3] + (j as f64 + 0.5) * gt[5]).collect(),
                (0..w).map(|i| gt[0] + (i as f64 + 0.5) * gt[1]).collect(),
            ];
            for (k, (n, _, size)) in dims.iter().enumerate() {
                let mut one = [hd[k]];
                let a = gdal_sys::GDALGroupCreateMDArray(root, c(n).as_ptr(), 1, one.as_mut_ptr(), f64t, null_mut());
                if a.is_null() {
                    return Err(format!("the coordinate {n}: {}", last_error()));
                }
                let (start, count) = ([0u64], [*size]);
                gdal_sys::GDALMDArrayWrite(
                    a,
                    start.as_ptr(),
                    count.as_ptr(),
                    null(),
                    null(),
                    f64t,
                    coords[k].as_ptr() as *const c_void,
                    coords[k].as_ptr() as *const c_void,
                    coords[k].len() * 8,
                );
                match k {
                    0 if info.time_is_date => {
                        attr(a, "units", "days since 1970-01-01 00:00:00");
                        attr(a, "calendar", "standard");
                        attr(a, "standard_name", "time");
                        attr(a, "axis", "T");
                    }
                    1 => {
                        attr(a, "standard_name", if geographic { "latitude" } else { "projection_y_coordinate" });
                        attr(a, "units", if geographic { "degrees_north" } else { "m" });
                        attr(a, "axis", "Y");
                    }
                    2 => {
                        attr(a, "standard_name", if geographic { "longitude" } else { "projection_x_coordinate" });
                        attr(a, "units", if geographic { "degrees_east" } else { "m" });
                        attr(a, "axis", "X");
                    }
                    _ => {}
                }
                gdal_sys::GDALMDArrayRelease(a);
            }
            let srs = SpatialRef::from_wkt(&info.crs_wkt).ok();
            let mut arrays = vec![];
            for n in names {
                let mut ao = null_mut();
                for o in &arr_opts {
                    ao = gdal_sys::CSLAddString(ao, c(o).as_ptr());
                }
                let a = gdal_sys::GDALGroupCreateMDArray(root, c(n).as_ptr(), 3, hd.as_mut_ptr(), f32t, ao);
                gdal_sys::CSLDestroy(ao);
                if a.is_null() {
                    return Err(format!("the variable {n}: {}", last_error()));
                }
                gdal_sys::GDALMDArraySetNoDataValueAsDouble(a, f64::NAN);
                if let Some(s) = &srs {
                    gdal_sys::GDALMDArraySetSpatialRef(a, s.to_c_hsrs());
                }
                attr(a, "long_name", n);
                arrays.push(a);
            }
            for dim in hd {
                gdal_sys::GDALDimensionRelease(dim);
            }
            gdal_sys::GDALExtendedDataTypeRelease(f64t);
            gdal_sys::GDALExtendedDataTypeRelease(st);
            gdal_sys::GDALGroupRelease(root);
            Ok(Sink::Multidim { ds, arrays, w, f32t })
        }
    }

    /// Rows by..by+r of date t's band b.
    fn write(&mut self, t: usize, b: usize, by: usize, r: usize, v: &[f32]) -> Result<(), String> {
        match self {
            Sink::Tiff { ds, .. } => {
                let w = v.len() / r.max(1);
                let mut band = ds.rasterband(t + 1).map_err(|e| e.to_string())?;
                let mut buf = gdal::raster::Buffer::new((w, r), v.to_vec());
                band.write((0, by as isize), (w, r), &mut buf).map_err(|e| e.to_string())
            }
            Sink::Multidim { arrays, w, f32t, .. } => {
                let (start, count) = ([t as u64, by as u64, 0], [1usize, r, *w]);
                // SAFETY: the array and the type are alive; the buffer holds count's values.
                let ok = unsafe {
                    gdal_sys::GDALMDArrayWrite(
                        arrays[b],
                        start.as_ptr(),
                        count.as_ptr(),
                        null(),
                        null(),
                        *f32t,
                        v.as_ptr() as *const c_void,
                        v.as_ptr() as *const c_void,
                        v.len() * 4,
                    )
                };
                if ok == 0 { Err(format!("writing date {}: {}", t + 1, last_error())) } else { Ok(()) }
            }
        }
    }

    /// Closes the dataset (the GeoTIFF made a COG at `out`).
    fn finish(
        self,
        out: &Path,
        threads: usize,
        progress: &(dyn Fn(f64) + Sync),
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        match self {
            Sink::Tiff { ds, temp } => {
                drop(ds);
                // the COG beside it, named as asked
                let dir = temp.with_extension("").with_extension("cogpart");
                std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                let src = dir.join(out.file_name().unwrap_or_default());
                std::fs::rename(&temp, src.with_extension("tif")).map_err(|e| e.to_string())?;
                let made = to_cogs(
                    &[src.with_extension("tif")],
                    out.parent().unwrap_or(Path::new(".")),
                    threads,
                    progress,
                    cancel,
                );
                let _ = std::fs::remove_dir_all(&dir);
                let made = made?;
                let got = PathBuf::from(&made[0]);
                if got != out {
                    std::fs::rename(&got, out).map_err(|e| e.to_string())?;
                }
                Ok(())
            }
            Sink::Multidim { ds, arrays, f32t, .. } => {
                // SAFETY: the handles made by multidim, released once.
                unsafe {
                    for a in arrays {
                        gdal_sys::GDALMDArrayRelease(a);
                    }
                    gdal_sys::GDALExtendedDataTypeRelease(f32t);
                    gdal_sys::GDALClose(ds);
                }
                if !last_error().is_empty() && !out.exists() {
                    return Err(format!("{}: {}", out.display(), last_error()));
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_names() {
        assert_eq!(key_of("LC08_220075_20200101_B4"), "LC08___B4");
        assert_eq!(key_of("S2_T23KLP_2020-01-01_B04"), "S2_T23KLP_2020-01-01_B04");
        assert_eq!(key_of("S2_20200101_B04"), "S2__B04");
        let n = band_names(&["S2__B04".into(), "S2__B08".into(), "S2__SCL".into()]);
        assert_eq!(n, vec!["B04", "B08", "SCL"]);
        assert_eq!(day_text(86400.0 * 18262.0), "2020-01-01");
    }

    /// Single-band files of two dates, two bands each, one date in two tiles: planned, built as VRTs and as COGs,
    /// the values where the files put them; then saved as a COG, a NetCDF and a Zarr and read back.
    #[test]
    fn build_and_save() {
        let dir = std::env::temp_dir().join(format!("janus_series_io_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let wkt = SpatialRef::from_epsg(32723).unwrap().to_wkt().unwrap();
        let mk = |name: &str, x0: f64, w: usize, v: f32| {
            let p = dir.join(name);
            let drv = gdal::DriverManager::get_driver_by_name("GTiff").unwrap();
            let mut d = drv.create_with_band_type::<f32, _>(&p, w, 4, 1).unwrap();
            d.set_geo_transform(&[x0, 30.0, 0.0, 7500000.0, 0.0, -30.0]).unwrap();
            d.set_projection(&wkt).unwrap();
            let mut b = d.rasterband(1).unwrap();
            let mut buf = gdal::raster::Buffer::new((w, 4), vec![v; w * 4]);
            b.write((0, 0), (w, 4), &mut buf).unwrap();
            p.to_string_lossy().into_owned()
        };
        // 2020-01-01: one tile of 6 x 4; 2020-02-01: two tiles of 3 x 4 side by side
        let files = vec![
            mk("s_20200101_red.tif", 600000.0, 6, 1.0),
            mk("s_20200101_nir.tif", 600000.0, 6, 2.0),
            mk("s_20200201_red.tif", 600000.0, 3, 3.0),
            mk("s_20200201_nir.tif", 600000.0, 3, 4.0),
            mk("s_20200201_red_b.tif", 600090.0, 3, 5.0),
            mk("s_20200201_nir_b.tif", 600090.0, 3, 6.0),
            mk("notes.tif", 600000.0, 6, 0.0),
        ];
        let mut p = plan(&files).unwrap();
        assert_eq!(p.skipped.len(), 1, "{:?}", p.skipped);
        // the second date's tiles have keys red / red_b: sorted they pair red with nir, red_b with nir_b
        assert_eq!(p.dates.len(), 2);
        assert_eq!(p.dates[1].bands.len(), 2);
        assert_eq!(p.bands, vec!["nir", "red"]);
        let g = p.grid.clone().unwrap();
        assert_eq!((g.w, g.h), (6, 4));
        let none = AtomicBool::new(false);
        let out = dir.join("vrt");
        let made = build(&p, &out, "s", BuildFormat::Vrt, 4, &|_, _| {}, &none).unwrap();
        assert_eq!(made.len(), 2);
        let read = |path: &str, b: usize| -> Vec<f32> {
            let d = Dataset::open(path).unwrap();
            let (w, h) = d.raster_size();
            d.rasterband(b).unwrap().read_as::<f32>((0, 0), (w, h), (w, h), None).unwrap().data().to_vec()
        };
        assert_eq!(read(&made[0], 1), vec![2.0; 24]);
        let v = read(&made[1], 2);
        assert_eq!((v[0], v[5]), (3.0, 5.0));
        assert_eq!(Dataset::open(&made[1]).unwrap().rasterband(1).unwrap().description().unwrap(), "nir");
        p.dates.truncate(2);
        let cogs = build(&p, &dir.join("cog"), "s", BuildFormat::Cog, 4, &|_, _| {}, &none).unwrap();
        assert!(cogs[1].ends_with("s_20200201.tif"), "{cogs:?}");
        assert_eq!(read(&cogs[1], 2), v);
        assert!(!dir.join("cog").join(".part").exists());
        // saved back: the series of the COGs (its first band shown)
        let info = Arc::new(janus_cube::open_cube(&cogs, &Default::default()).unwrap());
        assert_eq!(info.t(), 2);
        for format in [SaveFormat::Cog, SaveFormat::NetCdf, SaveFormat::Zarr] {
            let ext = match format {
                SaveFormat::Cog => "tif",
                SaveFormat::NetCdf => "nc",
                SaveFormat::Zarr => "zarr",
            };
            let path = dir.join(format!("saved.{ext}"));
            let opts = SaveOptions { format, name: "nir".into(), threads: 4, ..Default::default() };
            let s = save_series(&info, &opts, &path, &|_, _| {}, &none).unwrap();
            assert_eq!((s.dates, s.variables.clone()), (2, vec!["nir".to_string()]));
            if format == SaveFormat::Cog {
                let d = Dataset::open(&path).unwrap();
                assert_eq!(d.raster_count(), 2);
                assert_eq!(d.rasterband(2).unwrap().description().unwrap(), "2020-02-01");
                assert_eq!(read(&path.to_string_lossy(), 1), vec![2.0; 24]);
            } else {
                let back = janus_cube::open_cube(&[path.to_string_lossy().into_owned()], &Default::default()).unwrap();
                assert_eq!(back.t(), 2, "{}", back.description);
                assert!(back.time_is_date);
                assert_eq!(
                    back.layers.iter().map(|l| day_text(l.time)).collect::<Vec<_>>(),
                    ["2020-01-01", "2020-02-01"]
                );
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
