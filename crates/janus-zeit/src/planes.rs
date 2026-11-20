//! What the whole-window tools of bands and dates (MNF, CVA, iMAD, radiometric normalization) share: which dates and
//! bands a run reads, the lattice sample of the statistics, the files it writes (a multiband file, or a folder with a
//! file a date that opens as a series) and the small CSV writer of its tables.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use gdal::raster::{Buffer, RasterCreationOptions};
use gdal::{Dataset, DriverManager, Metadata};

use crate::job::{Inputs, JobSpec};

/// The lattice samples of some dates: for each date, [band][sample pixel].
pub(crate) type Lattice = Vec<Vec<Vec<f64>>>;

/// The nearest date of the series to a decimal year.
pub(crate) fn nearest_date(years: &[f64], year: f64) -> usize {
    (0..years.len()).min_by(|&a, &b| (years[a] - year).abs().total_cmp(&(years[b] - year).abs())).unwrap_or(0)
}

/// The date a date parameter names (the nearest one); 0 or less: `default`.
pub(crate) fn date_param(spec: &JobSpec, value: f64, default: usize) -> usize {
    if value > 0.0 && !spec.years.is_empty() { nearest_date(&spec.years, value) } else { default }
}

/// The date index a decimal year names in a list of years (the nearest); 0 or less: `default`.
pub(crate) fn date_index(years: &[f64], value: f64, default: usize) -> usize {
    if value > 0.0 && !years.is_empty() { nearest_date(years, value) } else { default }
}

/// The bands a run reads: the roles of the mapped bands (`b001`...) and their names.
#[derive(Clone, Debug)]
pub(crate) struct BandSet {
    pub roles: Vec<String>,
    pub names: Vec<String>,
}

fn is_band_role(k: &str) -> bool {
    k.len() >= 4 && k.starts_with('b') && k[1..].bytes().all(|c| c.is_ascii_digit())
}

/// The name of a band role.
pub(crate) fn name_of(spec: &JobSpec, role: &str) -> String {
    spec.band_names.get(role).cloned().unwrap_or_else(|| role.to_string())
}

/// The bands the text names (empty: every band of a date but the quality band), by name or by number (1 is the first
/// band of a date); at least `min` of them.
pub(crate) fn band_set(spec: &JobSpec, text: &str, min: usize, who: &str) -> Result<BandSet, String> {
    let all: Vec<String> = spec.bands.keys().filter(|k| is_band_role(k)).cloned().collect();
    if all.is_empty() {
        return Err(format!("ValueError: {who} reads every band of each date: this series has none"));
    }
    let tokens: Vec<&str> = text.split(',').map(str::trim).filter(|t| !t.is_empty()).collect();
    let roles: Vec<String> = if tokens.is_empty() {
        all
    } else {
        let mut out: Vec<String> = vec![];
        for t in tokens {
            let by_name = all.iter().find(|r| name_of(spec, r) == t);
            let by_number = t.parse::<usize>().ok().map(|n| format!("b{n:03}")).filter(|r| all.contains(r));
            let role = by_name
                .cloned()
                .or(by_number)
                .ok_or_else(|| format!("ValueError: bands: '{t}' is not a band of the series"))?;
            if !out.contains(&role) {
                out.push(role);
            }
        }
        out
    };
    if roles.len() < min {
        return Err(format!("ValueError: {who} needs {min} bands or more, {} chosen", roles.len()));
    }
    let names = roles.iter().map(|r| name_of(spec, r)).collect();
    Ok(BandSet { roles, names })
}

/// The window of the run.
pub(crate) fn window_of(spec: &JobSpec, inputs: &Inputs) -> [usize; 4] {
    let (iw, ih) = inputs.input.ds().raster_size();
    spec.window.unwrap_or([0, 0, iw, ih])
}

/// The stride of the lattice sample: every s-th row and column, `s = ceil(sqrt(pixels / sample))` (1 for 0: every
/// pixel).
pub(crate) fn lattice_stride(w: usize, h: usize, sample: i64) -> usize {
    if sample <= 0 { 1 } else { (((w * h) as f64 / sample as f64).sqrt().ceil() as usize).max(1) }
}

/// Rows of a strip of `nd` dates of `nb` bands over `w` columns (about 96 MB of float64).
pub(crate) fn strip_rows(nd: usize, nb: usize, w: usize, h: usize) -> usize {
    ((96usize << 20) / (8 * nd.max(1) * nb.max(1) * w.max(1))).clamp(1, 512).min(h.max(1))
}

/// The lattice sample of some dates: for each date, [band][sample pixel] (every `stride`-th row and column from the
/// window's first; NaN kept), and the number of sample pixels.
pub(crate) fn lattice(
    inputs: &mut Inputs,
    roles: &[String],
    dates: &[usize],
    window: [usize; 4],
    stride: usize,
    cancel: &AtomicBool,
    progress: &dyn Fn(f64),
) -> Result<(Lattice, usize), String> {
    let [x0, y0, x1, y1] = window;
    let (w, h) = (x1 - x0, y1 - y0);
    let rows = strip_rows(dates.len(), roles.len(), w, h);
    let mut out = vec![vec![vec![]; roles.len()]; dates.len()];
    let mut count = 0;
    let mut r = 0;
    while r < h {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let hh = rows.min(h - r);
        let picked: Vec<usize> = (r..r + hh).filter(|y| y % stride == 0).collect();
        if !picked.is_empty() {
            let planes = inputs.read_dates(roles, dates, x0, y0 + r, w, hh)?;
            for y in picked {
                for x in (0..w).step_by(stride) {
                    for (di, plane) in planes.iter().enumerate() {
                        for b in 0..roles.len() {
                            out[di][b].push(plane[b * w * hh + (y - r) * w + x]);
                        }
                    }
                    count += 1;
                }
            }
        }
        r += hh;
        progress(r as f64 / h as f64);
    }
    Ok((out, count))
}

/// The pixels where every band of both samples is finite, as rows `[x bands, y bands]` (float64).
pub(crate) fn valid_rows(x: &[Vec<f64>], y: &[Vec<f64>]) -> Vec<f64> {
    let n = x.len();
    let count = x.first().map_or(0, Vec::len);
    let mut z = Vec::new();
    for i in 0..count {
        if x.iter().chain(y).all(|b| b[i].is_finite()) {
            z.extend((0..n).map(|b| x[b][i]));
            z.extend((0..n).map(|b| y[b][i]));
        }
    }
    z
}

/// A run's date files' names: the dates in ISO form where the series has them, else the scene's number.
pub(crate) fn date_file_names(spec: &JobSpec) -> Vec<String> {
    let t = spec.years.len();
    match &spec.days {
        Some(days) if days.len() == t => {
            let mut names: Vec<String> = days
                .iter()
                .map(|&d| {
                    let (y, m, dd) = crate::numeric::civil(d);
                    format!("{y:04}-{m:02}-{dd:02}")
                })
                .collect();
            // two scenes of a day: the second gets its number
            for i in 0..t {
                if names[..i].contains(&names[i]) {
                    names[i] = format!("{}_{}", names[i], i + 1);
                }
            }
            names
        }
        _ => (0..t).map(|i| format!("scene_{:04}", i + 1)).collect(),
    }
}

/// GeoTIFFs of float32 bands on the window's grid, written strip by strip: one multiband file, or a file a date.
pub(crate) struct Tifs {
    w: usize,
    files: Vec<(PathBuf, Dataset)>,
}

impl Tifs {
    pub(crate) fn create(
        paths: Vec<PathBuf>,
        names: &[String],
        src: &Dataset,
        window: [usize; 4],
        spec: &JobSpec,
    ) -> Result<Tifs, String> {
        let (w, h) = (window[2] - window[0], window[3] - window[1]);
        let threads = match spec.threads {
            Some(n) if n > 0 => n.to_string(),
            _ => "ALL_CPUS".into(),
        };
        let driver = DriverManager::get_driver_by_name("GTiff").map_err(|e| e.to_string())?;
        let (gt, wkt) = (src.geo_transform().ok(), src.projection());
        let mut files = vec![];
        for path in paths {
            let mut opts = vec![
                "COMPRESS=DEFLATE".to_string(),
                "TILED=YES".into(),
                "BLOCKXSIZE=256".into(),
                "BLOCKYSIZE=256".into(),
                "BIGTIFF=IF_SAFER".into(),
                format!("NUM_THREADS={threads}"),
            ];
            if names.len() > 1 {
                opts.push("INTERLEAVE=BAND".into());
            }
            let options = RasterCreationOptions::from_iter(opts);
            let mut ds = driver
                .create_with_band_type_with_options::<f32, _>(&path, w, h, names.len(), &options)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            if let Some(g) = gt {
                let x = g[0] + window[0] as f64 * g[1] + window[1] as f64 * g[2];
                let y = g[3] + window[0] as f64 * g[4] + window[1] as f64 * g[5];
                ds.set_geo_transform(&[x, g[1], g[2], y, g[4], g[5]]).map_err(|e| e.to_string())?;
            }
            if !wkt.is_empty() {
                ds.set_projection(&wkt).map_err(|e| e.to_string())?;
            }
            for (b, name) in names.iter().enumerate() {
                let mut band = ds.rasterband(b + 1).map_err(|e| e.to_string())?;
                band.set_no_data_value(Some(f64::NAN)).map_err(|e| e.to_string())?;
                band.set_description(name).map_err(|e| e.to_string())?;
            }
            files.push((path, ds));
        }
        Ok(Tifs { w, files })
    }

    /// The rows `row..row + rows` of file `file`: `planes` is [band][rows x w].
    pub(crate) fn write(&mut self, file: usize, row: usize, rows: usize, planes: &[f32]) -> Result<(), String> {
        let w = self.w;
        let (path, ds) = &mut self.files[file];
        let count = ds.raster_count();
        if planes.len() != count * w * rows {
            return Err(format!("{}: {} values for {count} bands of {w} x {rows}", path.display(), planes.len()));
        }
        for b in 0..count {
            let part = planes[b * w * rows..(b + 1) * w * rows].to_vec();
            let mut band = ds.rasterband(b + 1).map_err(|e| e.to_string())?;
            band.write((0, row as isize), (w, rows), &mut Buffer::new((w, rows), part))
                .map_err(|e| format!("{}: {e}", path.display()))?;
        }
        Ok(())
    }
}

/// A CSV field: quoted when it holds a comma, a quote or a line break.
pub(crate) fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() }
}

/// A number for a table: empty when it is not finite.
pub(crate) fn csv_num(v: f64) -> String {
    if v.is_finite() { format!("{v}") } else { String::new() }
}

/// Writes a CSV: the header, then the rows.
pub(crate) fn write_csv(path: &Path, header: &[&str], rows: &[Vec<String>]) -> Result<(), String> {
    let mut text = header.join(",") + "\n";
    for r in rows {
        text += &r.iter().map(|f| csv_field(f)).collect::<Vec<_>>().join(",");
        text.push('\n');
    }
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

/// A rasterized file's mask on the window (1 inside): a mask raster on the series' grid (nonzero is inside), or a
/// polygon file burned on the window's grid.
pub(crate) fn region_mask(
    file: &str,
    src: &Dataset,
    window: [usize; 4],
    dir: &Path,
    cancel: &AtomicBool,
) -> Result<Vec<u8>, String> {
    let (w, h) = (window[2] - window[0], window[3] - window[1]);
    if let Ok(ds) = Dataset::open(file)
        && ds.raster_count() > 0
    {
        let (sw, sh) = src.raster_size();
        if ds.raster_size() != (sw, sh) {
            return Err(format!(
                "ValueError: the noise region {file} must be on the series' grid ({sw} x {sh} pixels)"
            ));
        }
        let v = crate::job::read(&ds, 1..=1, window[0], window[1], w, h, false)?;
        return Ok(v.iter().map(|&x| u8::from(x.is_finite() && x != 0.0)).collect());
    }
    let g = src.geo_transform().map_err(|e| format!("ValueError: the series has no geotransform ({e})"))?;
    let grid = crate::warp::Grid {
        wkt: src.projection(),
        gt: [
            g[0] + window[0] as f64 * g[1] + window[1] as f64 * g[2],
            g[1],
            g[2],
            g[3] + window[0] as f64 * g[4] + window[1] as f64 * g[5],
            g[4],
            g[5],
        ],
        w,
        h,
    };
    let out = dir.join("noise_region.tif");
    let opts = crate::rasterize::RasterizeOptions {
        layer: String::new(),
        burn: crate::rasterize::Burn::One,
        all_touched: false,
        grid,
    };
    crate::rasterize::rasterize(file, &opts, &out, &|_, _| {}, cancel)?;
    let ds = Dataset::open(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let v = crate::job::read(&ds, 1..=1, 0, 0, w, h, false)?;
    let _ = std::fs::remove_file(&out);
    Ok(v.iter().map(|&x| u8::from(x.is_finite() && x != 0.0)).collect())
}
