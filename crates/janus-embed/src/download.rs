//! Embeddings of an area downloaded as a layer (what Zeit's `load_embeddings` and the bridge's task_embeddings.py
//! did for Janus): one GeoTIFF per year, Int16 with a scale per band and nodata -32768, pixel-interleaved, tiled
//! 256 x 256, DEFLATE, the dimensions' names as band descriptions and Zeit's ZEIT_EMBEDDING tag.
//!
//! AlphaEarth is read natively (zeit/_alphaearth.py): the open copy on Source Cooperative holds, per year and UTM
//! zone, COGs of 8192 x 8192 cells and 64 int8 bands, indexed by aef_index.parquet; the files are bottom-up (the
//! first row is the southern one), values map to [-1, 1] as sign(v) * (v / 127.5)^2 and -128 marks a pixel without
//! an embedding. TESSERA is read natively too, from its Zarr store (tessera.rs).

use std::ffi::CString;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use gdal::Dataset;
use gdal::spatial_ref::{AxisMappingStrategy, CoordTransform, SpatialRef};

/// The bucket's own endpoint rather than the data.source.coop gateway, which drops requests under the load of a
/// region read.
const ROOT: &str = "https://s3.us-west-2.amazonaws.com/us-west-2.opendata.source.coop/tge-labs/aef/v1/annual";
const INDEX: &str = "aef_index.parquet";
const REFRESH_DAYS: u64 = 30;
const ATTRIBUTION: &str =
    "The AlphaEarth Foundations Satellite Embedding dataset is produced by Google and Google DeepMind.";
/// cells a side of the files' own blocks
const CHUNK: usize = 1024;
/// bands of a read: each band is a request of its own, and the groups go in parallel
const BAND_GROUP: usize = 8;
const NODATA: i16 = -32768;
/// what the GeoTIFFs keep exactly enough (|v| <= range): AlphaEarth's vectors have norm 1
const ALPHAEARTH_RANGE: f64 = 1.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    AlphaEarth,
    Tessera,
}

impl Source {
    pub fn id(self) -> &'static str {
        match self {
            Source::AlphaEarth => "alphaearth",
            Source::Tessera => "tessera",
        }
    }

    pub fn dims(self) -> usize {
        match self {
            Source::AlphaEarth => 64,
            Source::Tessera => 128,
        }
    }
}

pub struct DownloadSpec {
    pub source: Source,
    pub years: Vec<i32>,
    /// west, south, east, north in longitude and latitude
    pub bounds: [f64; 4],
    /// metres; None: the native 10 m
    pub res: Option<f64>,
    pub out_dir: PathBuf,
    /// where the AlphaEarth index is kept
    pub cache_dir: PathBuf,
}

/// Runs a download: `progress(fraction, message)`; Err("cancelled") when `cancel` was set. Returns the files.
pub fn download(
    spec: &DownloadSpec,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<Vec<PathBuf>, String> {
    std::fs::create_dir_all(&spec.out_dir).map_err(|e| format!("{}: {e}", spec.out_dir.display()))?;
    match spec.source {
        Source::AlphaEarth => alphaearth(spec, progress, cancel),
        Source::Tessera => crate::tessera::download(spec, progress, cancel),
    }
}

fn set_config(k: &str, v: &str) {
    let _ = gdal::config::set_config_option(k, v);
}

/// GDAL's HTTP reads of COGs: no directory listing, retries, a cache, merged ranges.
pub(crate) fn http_options() {
    for (k, v) in [
        ("GDAL_DISABLE_READDIR_ON_OPEN", "EMPTY_DIR"),
        ("CPL_VSIL_CURL_ALLOWED_EXTENSIONS", ".tiff,.tif,.parquet,.json,{noext}"),
        ("GDAL_HTTP_MAX_RETRY", "5"),
        ("GDAL_HTTP_RETRY_DELAY", "1"),
        ("VSI_CACHE", "TRUE"),
        ("GDAL_HTTP_MULTIRANGE", "YES"),
        ("GDAL_HTTP_MERGE_CONSECUTIVE_RANGES", "YES"),
    ] {
        set_config(k, v);
    }
}

/// A file over HTTP copied to `path` (through GDAL's /vsicurl/, the program's libcurl), through a temporary file.
fn fetch(url: &str, path: &Path, progress: &dyn Fn(f64), cancel: &AtomicBool) -> Result<(), String> {
    use std::io::Write;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("part");
    let src = CString::new(format!("/vsicurl/{url}")).unwrap();
    let mode = CString::new("rb").unwrap();
    unsafe {
        let f = gdal_sys::VSIFOpenL(src.as_ptr(), mode.as_ptr());
        if f.is_null() {
            return Err(format!("could not reach {url}"));
        }
        let size = janus_gdal::vsi_stat(&format!("/vsicurl/{url}")).map_or(0, |s| s.0);
        let mut out = File::create(&tmp).map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; 1 << 20];
        let mut got = 0u64;
        loop {
            if cancel.load(Ordering::Relaxed) {
                gdal_sys::VSIFCloseL(f);
                let _ = std::fs::remove_file(&tmp);
                return Err("cancelled".into());
            }
            let n = gdal_sys::VSIFReadL(buf.as_mut_ptr().cast(), 1, buf.len(), f);
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            got += n as u64;
            if size > 0 {
                progress(got as f64 / size as f64);
            }
        }
        gdal_sys::VSIFCloseL(f);
        if size > 0 && got != size {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("{url}: the download stopped at {got} of {size} bytes"));
        }
    }
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// A file of the AlphaEarth index.
#[derive(Clone, Debug)]
struct IndexRow {
    /// the path under the root: <year>/<zone>/<name>.tiff
    rel: String,
    year: i32,
    zone: String,
    crs: String,
    /// west, south, east, north in the zone's UTM, and in longitude and latitude
    utm: [f64; 4],
    wgs84: [f64; 4],
}

/// The index read last: its file, modification time and rows.
type IndexCache = Option<(PathBuf, std::time::SystemTime, std::sync::Arc<Vec<IndexRow>>)>;

/// The index read from the parquet file (kept for the process, by file and modification time).
fn read_index(path: &Path) -> Result<Vec<IndexRow>, String> {
    use parquet::file::reader::{FileReader, SerializedFileReader};
    use parquet::record::Field;
    use parquet::schema::types::Type;
    static CACHE: OnceLock<Mutex<IndexCache>> = OnceLock::new();
    let mtime = std::fs::metadata(path).and_then(|m| m.modified()).map_err(|e| e.to_string())?;
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    if let Some((p, t, rows)) = cache.lock().unwrap().as_ref()
        && p == path
        && *t == mtime
    {
        return Ok(rows.as_ref().clone());
    }
    let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let reader = SerializedFileReader::new(file).map_err(|e| format!("{}: {e}", path.display()))?;
    const COLUMNS: [&str; 12] = [
        "path",
        "year",
        "utm_zone",
        "crs",
        "utm_west",
        "utm_south",
        "utm_east",
        "utm_north",
        "wgs84_west",
        "wgs84_south",
        "wgs84_east",
        "wgs84_north",
    ];
    let root = reader.metadata().file_metadata().schema_descr().root_schema().clone();
    let fields: Vec<_> = root.get_fields().iter().filter(|f| COLUMNS.contains(&f.name())).cloned().collect();
    let projection = Type::group_type_builder("schema").with_fields(fields).build().map_err(|e| e.to_string())?;
    let mut rows = Vec::new();
    for row in reader.get_row_iter(Some(projection)).map_err(|e| e.to_string())? {
        let row = row.map_err(|e| e.to_string())?;
        let mut r = IndexRow {
            rel: String::new(),
            year: 0,
            zone: String::new(),
            crs: String::new(),
            utm: [0.0; 4],
            wgs84: [0.0; 4],
        };
        for (name, f) in row.get_column_iter() {
            let num = || match f {
                Field::Double(v) => *v,
                Field::Long(v) => *v as f64,
                Field::Int(v) => f64::from(*v),
                _ => f64::NAN,
            };
            let text = || match f {
                Field::Str(s) => s.clone(),
                _ => String::new(),
            };
            match name.as_str() {
                "path" => {
                    let p = text();
                    r.rel = p.split_once("/annual/").map_or(p.clone(), |(_, rel)| rel.to_string());
                }
                "year" => r.year = num() as i32,
                "utm_zone" => r.zone = text(),
                "crs" => r.crs = text(),
                "utm_west" => r.utm[0] = num(),
                "utm_south" => r.utm[1] = num(),
                "utm_east" => r.utm[2] = num(),
                "utm_north" => r.utm[3] = num(),
                "wgs84_west" => r.wgs84[0] = num(),
                "wgs84_south" => r.wgs84[1] = num(),
                "wgs84_east" => r.wgs84[2] = num(),
                "wgs84_north" => r.wgs84[3] = num(),
                _ => {}
            }
        }
        rows.push(r);
    }
    *cache.lock().unwrap() = Some((path.to_path_buf(), mtime, std::sync::Arc::new(rows.clone())));
    Ok(rows)
}

fn srs(def: &str) -> Result<SpatialRef, String> {
    let mut s = SpatialRef::from_definition(def).map_err(|e| format!("{def}: {e}"))?;
    s.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    Ok(s)
}

/// The box in another CRS (densified edges, as rasterio's transform_bounds).
pub(crate) fn bounds_in(bbox: [f64; 4], crs: &str) -> Result<[f64; 4], String> {
    let (wgs, dst) = (srs("EPSG:4326")?, srs(crs)?);
    let ct = CoordTransform::new(&wgs, &dst).map_err(|e| e.to_string())?;
    ct.transform_bounds(&bbox, 21).map_err(|e| e.to_string())
}

/// int8 values as the embedding's [-1, 1]; NaN for -128 (no embedding).
fn dequantise(v: i8) -> f32 {
    if v == -128 {
        return f32::NAN;
    }
    let x = f32::from(v) / 127.5;
    x * x * f32::from(v).signum()
}

/// The meta of AlphaEarth (zeit/_alphaearth.py, _meta), the GeoTIFFs' ZEIT_EMBEDDING tag.
fn alphaearth_tag() -> String {
    serde_json::json!({
        "embedding_source": "alphaearth",
        "embedding_version": "1",
        "embedding_variant": "annual",
        "embedding_model": "AlphaEarth Foundations",
        "embedding_dimensions": 64,
        "embedding_license": "CC-BY-4.0",
        "embedding_attribution": ATTRIBUTION,
        "embedding_backend": "source.coop",
        "embedding_citation": "Brown et al. (2025), AlphaEarth Foundations: An embedding field model for accurate and \
                               efficient global mapping from sparse label data, arXiv:2507.22291",
    })
    .to_string()
}

/// A year's files of a zone: each one's location and where its top-left cell is on the zone's grid.
struct ZoneFile {
    location: String,
    row: i64,
    col: i64,
}

fn alphaearth(
    spec: &DownloadSpec,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<Vec<PathBuf>, String> {
    http_options();
    // the file index (78 MB), downloaded once a month
    let index_path = spec.cache_dir.join("alphaearth").join(INDEX);
    let stale = std::fs::metadata(&index_path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_none_or(|age| age.as_secs() > REFRESH_DAYS * 86400);
    if stale {
        progress(0.0, "the AlphaEarth file index (78 MB), downloaded once a month");
        let r = fetch(
            &format!("{ROOT}/{INDEX}"),
            &index_path,
            &|f| progress(0.05 * f, "the AlphaEarth file index (78 MB)"),
            cancel,
        );
        // offline: the copy we have still lists the files
        if let Err(e) = r
            && (e == "cancelled" || !index_path.exists())
        {
            return Err(e);
        }
    }
    progress(0.05, "finding the data");
    let index = read_index(&index_path)?;
    let mut available: Vec<i32> = index.iter().map(|r| r.year).collect();
    available.sort_unstable();
    available.dedup();
    let missing: Vec<i32> = spec.years.iter().copied().filter(|y| !available.contains(y)).collect();
    if !missing.is_empty() {
        return Err(format!("AlphaEarth has the years {available:?}, not {missing:?}"));
    }
    let [w, s, e, n] = spec.bounds;
    let hit: Vec<&IndexRow> = index
        .iter()
        .filter(|r| {
            spec.years.contains(&r.year) && r.wgs84[0] < e && r.wgs84[2] > w && r.wgs84[1] < n && r.wgs84[3] > s
        })
        .collect();
    let mut zones: Vec<&str> = hit.iter().map(|r| r.zone.as_str()).collect();
    zones.sort_unstable();
    zones.dedup();
    if zones.is_empty() {
        return Err(format!("no AlphaEarth embeddings cover the region ({w:.5}, {s:.5}, {e:.5}, {n:.5})"));
    }
    if zones.len() > 1 {
        return Err(format!(
            "the region spans {} UTM zones ({}): download each side on its own",
            zones.len(),
            zones.join(", ")
        ));
    }
    let files: Vec<&&IndexRow> = hit.iter().filter(|r| r.zone == zones[0]).collect();
    let crs = files[0].crs.clone();
    // the files of a zone lie on one grid of 10 m cells
    let left = files.iter().map(|r| r.utm[0]).fold(f64::INFINITY, f64::min);
    let top = files.iter().map(|r| r.utm[3]).fold(f64::NEG_INFINITY, f64::max);
    let right = files.iter().map(|r| r.utm[2]).fold(f64::NEG_INFINITY, f64::max);
    let bottom = files.iter().map(|r| r.utm[1]).fold(f64::INFINITY, f64::min);
    let (zw, zh) = (((right - left) / 10.0).round() as i64, ((top - bottom) / 10.0).round() as i64);
    let b = bounds_in(spec.bounds, &crs)?;
    // the output grid: the zone's cells the region touches, or cells of `res` on multiples of it (the mean of the
    // 10 m cells under each)
    let res = spec.res.filter(|&r| r > 10.0).unwrap_or(10.0);
    let k = (res / 10.0).round().max(1.0) as usize;
    let (x0, y0, ow, oh) = if k == 1 {
        let col0 = (((b[0] - left) / 10.0 + 1e-6).floor() as i64).max(0);
        let col1 = (((b[2] - left) / 10.0 - 1e-6).ceil() as i64).min(zw);
        let row0 = (((top - b[3]) / 10.0 + 1e-6).floor() as i64).max(0);
        let row1 = (((top - b[1]) / 10.0 - 1e-6).ceil() as i64).min(zh);
        if col1 <= col0 || row1 <= row0 {
            return Err("the region misses the AlphaEarth files".into());
        }
        (left + 10.0 * col0 as f64, top - 10.0 * row0 as f64, (col1 - col0) as usize, (row1 - row0) as usize)
    } else {
        let x0 = (b[0] / res + 1e-6).floor() * res;
        let y0 = (b[3] / res - 1e-6).ceil() * res;
        let ow = (((b[2] - x0) / res - 1e-6).ceil() as usize).max(1);
        let oh = (((y0 - b[1]) / res - 1e-6).ceil() as usize).max(1);
        (x0, y0, ow, oh)
    };
    // the zone's cells of the output's first one (10 m cells: whole numbers)
    let (zc0, zr0) = (((x0 - left) / 10.0).round() as i64, ((top - y0) / 10.0).round() as i64);
    let tag = alphaearth_tag();
    let scale = ALPHAEARTH_RANGE / 32767.0;
    let mut outputs = Vec::new();
    // strips of whole blocks of the files (rows of the zone's grid on multiples of CHUNK)
    let strip_rows = (CHUNK / k).max(1);
    let strips: Vec<(usize, usize)> = (0..oh).step_by(strip_rows).map(|r| (r, strip_rows.min(oh - r))).collect();
    let total = spec.years.len() * strips.len();
    let done = AtomicUsize::new(0);
    let t0 = Instant::now();
    for (yi, &year) in spec.years.iter().enumerate() {
        let zf: Vec<ZoneFile> = files
            .iter()
            .filter(|r| r.year == year)
            .map(|r| ZoneFile {
                location: format!("/vsicurl/{ROOT}/{}", r.rel),
                row: ((top - r.utm[3]) / 10.0).round() as i64,
                col: ((r.utm[0] - left) / 10.0).round() as i64,
            })
            .collect();
        let path = spec.out_dir.join(format!("alphaearth_{year}.tif"));
        let mut out = GeoTiff::create(&path, ow, oh, 64, [x0, res, 0.0, y0, 0.0, -res], &crs, scale, &tag)?;
        for &(r, h) in &strips {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            // the 10 m cells under the strip: rows zr0 + r * k .., every column
            let (sr0, sh, sc0, sw) = (zr0 + (r * k) as i64, h * k, zc0, ow * k);
            let cells = read_cells(&zf, sr0, sh, sc0, sw, cancel)?;
            let block = if k == 1 { cells } else { average(&cells, 64, sh, sw, k) };
            out.write_strip(r, h, &block)?;
            let d = done.fetch_add(1, Ordering::Relaxed) + 1;
            let el = t0.elapsed().as_secs_f64();
            progress(
                0.05 + 0.95 * d as f64 / total as f64,
                &format!(
                    "{year} ({}/{}): rows {}/{oh}, ~{:.0} s left",
                    yi + 1,
                    spec.years.len(),
                    r + h,
                    el / d as f64 * (total - d) as f64
                ),
            );
        }
        out.finish()?;
        outputs.push(path);
    }
    Ok(outputs)
}

/// The 10 m cells (rows r0 .. r0 + h, columns c0 .. c0 + w of the zone's grid) of the year's files, [band][row][col]
/// as float (NaN where none): each file's part read on its own thread, its rows flipped (the files are bottom-up).
fn read_cells(
    files: &[ZoneFile],
    r0: i64,
    h: usize,
    c0: i64,
    w: usize,
    cancel: &AtomicBool,
) -> Result<Vec<f32>, String> {
    let out = Mutex::new(vec![f32::NAN; 64 * h * w]);
    let first_error: Mutex<Option<String>> = Mutex::new(None);
    // the files' parts, the blocks of CHUNK columns of each, groups of bands: tasks for several threads (the
    // requests over the network wait more than they compute)
    let mut tasks: Vec<(usize, i64, i64, usize)> = Vec::new();
    for (fi, f) in files.iter().enumerate() {
        let (a, b) = (c0.max(f.col), (c0 + w as i64).min(f.col + 8192));
        let mut c = a;
        while c < b {
            let e = ((c / CHUNK as i64 + 1) * CHUNK as i64).min(b);
            for g in (0..64).step_by(BAND_GROUP) {
                tasks.push((fi, c, e, g));
            }
            c = e;
        }
    }
    let next = AtomicUsize::new(0);
    std::thread::scope(|sc| {
        for _ in 0..16.min(tasks.len()) {
            sc.spawn(|| {
                let mut open: Option<(usize, Dataset)> = None;
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= tasks.len() || cancel.load(Ordering::Relaxed) || first_error.lock().unwrap().is_some() {
                        break;
                    }
                    let (fi, ca, cb, g) = tasks[i];
                    let f = &files[fi];
                    if open.as_ref().is_none_or(|o| o.0 != fi) {
                        match Dataset::open(&f.location) {
                            Ok(ds) => open = Some((fi, ds)),
                            Err(e) => {
                                *first_error.lock().unwrap() = Some(format!("{}: {e}", f.location));
                                break;
                            }
                        }
                    }
                    let ds = &open.as_ref().unwrap().1;
                    let (fw, fh) = ds.raster_size();
                    let (ra, rb) = (r0.max(f.row), (r0 + h as i64).min(f.row + fh as i64));
                    let cb = cb.min(f.col + fw as i64);
                    if ra >= rb || ca >= cb {
                        continue;
                    }
                    let up = ds.geo_transform().is_ok_and(|g| g[5] > 0.0); // bottom-up: file row 0 is the southern one
                    let (rh, cw) = ((rb - ra) as usize, (cb - ca) as usize);
                    let file_row0 = if up { fh as i64 - (rb - f.row) } else { ra - f.row };
                    let mut buf = vec![0i8; BAND_GROUP * rh * cw];
                    let mut bands: Vec<i32> = (g as i32 + 1..=(g + BAND_GROUP) as i32).collect();
                    let err = unsafe {
                        gdal_sys::GDALDatasetRasterIO(
                            ds.c_dataset(),
                            gdal_sys::GDALRWFlag::GF_Read,
                            (ca - f.col) as i32,
                            file_row0 as i32,
                            cw as i32,
                            rh as i32,
                            buf.as_mut_ptr().cast(),
                            cw as i32,
                            rh as i32,
                            gdal_sys::GDALDataType::GDT_Int8,
                            BAND_GROUP as i32,
                            bands.as_mut_ptr(),
                            0,
                            0,
                            0,
                        )
                    };
                    if err != gdal_sys::CPLErr::CE_None {
                        let m = unsafe { std::ffi::CStr::from_ptr(gdal_sys::CPLGetLastErrorMsg()) }
                            .to_string_lossy()
                            .into_owned();
                        *first_error.lock().unwrap() = Some(format!("{}: {m}", f.location));
                        break;
                    }
                    let mut o = out.lock().unwrap();
                    for j in 0..BAND_GROUP {
                        for y in 0..rh {
                            let src_y = if up { rh - 1 - y } else { y };
                            let dst = (g + j) * h * w + (ra - r0) as usize * w + y * w + (ca - c0) as usize;
                            let src = j * rh * cw + src_y * cw;
                            for x in 0..cw {
                                o[dst + x] = dequantise(buf[src + x]);
                            }
                        }
                    }
                }
            });
        }
    });
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    if let Some(e) = first_error.into_inner().unwrap() {
        return Err(e);
    }
    Ok(out.into_inner().unwrap())
}

/// The mean of each k x k block of cells ([band][h][w] -> [band][h/k][w/k]), NaN left out.
pub(crate) fn average(cells: &[f32], d: usize, h: usize, w: usize, k: usize) -> Vec<f32> {
    let (oh, ow) = (h / k, w / k);
    let mut out = vec![f32::NAN; d * oh * ow];
    for b in 0..d {
        for y in 0..oh {
            for x in 0..ow {
                let (mut s, mut n) = (0f64, 0usize);
                for dy in 0..k {
                    let row = &cells[b * h * w + (y * k + dy) * w + x * k..][..k];
                    for &v in row.iter().filter(|v| v.is_finite()) {
                        s += f64::from(v);
                        n += 1;
                    }
                }
                if n > 0 {
                    out[b * oh * ow + y * ow + x] = (s / n as f64) as f32;
                }
            }
        }
    }
    out
}

/// A year's GeoTIFF being written (the bridge's profile).
pub(crate) struct GeoTiff {
    ds: Dataset,
    path: PathBuf,
    tmp: PathBuf,
    w: usize,
    d: usize,
    scale: f64,
}

impl GeoTiff {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn create(
        path: &Path,
        w: usize,
        h: usize,
        d: usize,
        gt: [f64; 6],
        crs: &str,
        scale: f64,
        tag: &str,
    ) -> Result<GeoTiff, String> {
        use gdal::Metadata;
        use gdal::raster::RasterCreationOptions;
        let drv = gdal::DriverManager::get_driver_by_name("GTiff").map_err(|e| e.to_string())?;
        let opts = RasterCreationOptions::from_iter([
            "TILED=YES",
            "BLOCKXSIZE=256",
            "BLOCKYSIZE=256",
            "COMPRESS=DEFLATE",
            "PREDICTOR=2",
            "INTERLEAVE=PIXEL",
            "BIGTIFF=IF_SAFER",
        ]);
        let tmp = PathBuf::from(format!("{}.part", path.display()));
        let mut ds = drv
            .create_with_band_type_with_options::<i16, _>(&tmp, w, h, d, &opts)
            .map_err(|e| format!("could not create {}: {e}", tmp.display()))?;
        ds.set_geo_transform(&gt).map_err(|e| e.to_string())?;
        if let Ok(s) = srs(crs)
            && let Ok(wkt) = s.to_wkt()
        {
            let _ = ds.set_projection(&wkt);
        }
        let _ = ds.set_metadata_item("ZEIT_EMBEDDING", tag, "");
        for b in 0..d {
            let mut band = ds.rasterband(b + 1).map_err(|e| e.to_string())?;
            let _ = band.set_no_data_value(Some(f64::from(NODATA)));
            let _ = band.set_description(&format!("A{b:02}"));
            unsafe {
                gdal_sys::GDALSetRasterScale(band.c_rasterband(), scale);
                gdal_sys::GDALSetRasterOffset(band.c_rasterband(), 0.0);
            }
        }
        Ok(GeoTiff { ds, path: path.to_path_buf(), tmp, w, d, scale })
    }

    /// Rows r .. r + h: [band][row][col] values quantised to Int16 (NaN -> nodata).
    pub(crate) fn write_strip(&mut self, r: usize, h: usize, block: &[f32]) -> Result<(), String> {
        let plane = self.w * h;
        let mut q = vec![0i16; self.d * plane];
        for (o, &v) in q.iter_mut().zip(block) {
            *o = if v.is_finite() {
                (v / self.scale as f32).round_ties_even().clamp(-32767.0, 32767.0) as i16
            } else {
                NODATA
            };
        }
        let mut bands: Vec<i32> = (1..=self.d as i32).collect();
        let err = unsafe {
            gdal_sys::GDALDatasetRasterIO(
                self.ds.c_dataset(),
                gdal_sys::GDALRWFlag::GF_Write,
                0,
                r as i32,
                self.w as i32,
                h as i32,
                q.as_mut_ptr().cast(),
                self.w as i32,
                h as i32,
                gdal_sys::GDALDataType::GDT_Int16,
                self.d as i32,
                bands.as_mut_ptr(),
                0,
                0,
                0,
            )
        };
        if err != gdal_sys::CPLErr::CE_None {
            return Err(format!("could not write {}", self.tmp.display()));
        }
        Ok(())
    }

    pub(crate) fn finish(self) -> Result<(), String> {
        let GeoTiff { ds, path, tmp, .. } = self;
        drop(ds);
        let _ = std::fs::remove_file(&path);
        std::fs::rename(&tmp, &path).map_err(|e| format!("cannot rename {}: {e}", tmp.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Zeit's cached copy of the index, if this machine has one.
    #[test]
    fn index_of_zeit_cache() {
        let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) else { return };
        let p = PathBuf::from(home).join(".cache/zeit/embeddings/alphaearth").join(INDEX);
        if !p.exists() {
            return;
        }
        let t = Instant::now();
        let rows = read_index(&p).unwrap();
        println!(
            "{} files in the index, read in {:.0} ms; {:?}",
            rows.len(),
            t.elapsed().as_secs_f64() * 1000.0,
            rows[0]
        );
        assert!(rows.len() > 100_000);
        assert!(rows.iter().all(|r| r.rel.ends_with(".tiff") && r.year >= 2017 && r.crs.starts_with("EPSG:")));
    }

    /// The native download against the bridge's (Zeit in Python) files of the same area, value by value: run with
    /// `cargo test -p janus-embed --release -- --ignored against_the_bridge` (network; the bridge's folder in the app
    /// data's results).
    #[test]
    #[ignore]
    fn against_the_bridge() {
        let Some(local) = std::env::var_os("LOCALAPPDATA") else { return };
        let dir = PathBuf::from(local).join("Janus/results/embeddings/alphaearth_2023-2024_20261010_020020");
        let Ok(job) = std::fs::read_to_string(dir.join("job.json")) else { return };
        let job: serde_json::Value = serde_json::from_str(&job).unwrap();
        let b: Vec<f64> = job["params"]["bounds"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
        let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).unwrap();
        let out = std::env::temp_dir().join("janus-embed-download-test");
        let _ = std::fs::remove_dir_all(&out);
        let spec = DownloadSpec {
            source: Source::AlphaEarth,
            years: vec![2023, 2024],
            bounds: [b[0], b[1], b[2], b[3]],
            res: None,
            out_dir: out.clone(),
            cache_dir: PathBuf::from(home).join(".cache/zeit/embeddings"),
        };
        let t = Instant::now();
        let files = download(&spec, &|f, m| println!("{:3.0}% {m}", f * 100.0), &AtomicBool::new(false)).unwrap();
        println!("{} files in {:.1} s", files.len(), t.elapsed().as_secs_f64());
        for f in &files {
            let name = f.file_name().unwrap();
            let (a, b) = (Dataset::open(f).unwrap(), Dataset::open(dir.join(name)).unwrap());
            assert_eq!(a.raster_size(), b.raster_size(), "{name:?}");
            assert_eq!(a.geo_transform().unwrap(), b.geo_transform().unwrap());
            let (w, h) = a.raster_size();
            let mut differ = 0;
            for band in 1..=64 {
                let ra = a.rasterband(band).unwrap().read_as::<i16>((0, 0), (w, h), (w, h), None).unwrap();
                let rb = b.rasterband(band).unwrap().read_as::<i16>((0, 0), (w, h), (w, h), None).unwrap();
                differ += ra.data().iter().zip(rb.data()).filter(|(x, y)| x != y).count();
            }
            println!("{name:?}: {w} x {h} x 64, {differ} values differ");
            assert_eq!(differ, 0);
        }
        let _ = std::fs::remove_dir_all(&out);
    }

    /// A small area of both sources over the network (whichever GDAL is loaded: one without blosc reads TESSERA
    /// through blosc.rs): `cargo test -p janus-embed --release -- --ignored small_area`.
    #[test]
    #[ignore]
    fn small_area() {
        for source in [Source::AlphaEarth, Source::Tessera] {
            let out = std::env::temp_dir().join("janus-embed-small-area");
            let _ = std::fs::remove_dir_all(&out);
            let spec = DownloadSpec {
                source,
                years: vec![2024],
                bounds: [-46.66, -23.57, -46.64, -23.55],
                res: None,
                out_dir: out.clone(),
                cache_dir: std::env::temp_dir().join("janus-embed-small-area-cache"),
            };
            let files = download(&spec, &|_, _| {}, &AtomicBool::new(false)).unwrap();
            let d = Dataset::open(&files[0]).unwrap();
            println!("{source:?}: {:?}, {} bands", d.raster_size(), d.raster_count());
            assert!(d.raster_count() >= 64);
            let _ = std::fs::remove_dir_all(&out);
        }
    }

    /// TESSERA read natively against Zeit's files of the same region (task_embeddings.py through Zeit, in the folder
    /// JANUS_TESSERA_REF with its job.json), value by value: `cargo test -p janus-embed --release -- --ignored
    /// tessera_against_zeit` (network).
    #[test]
    #[ignore]
    fn tessera_against_zeit() {
        let Some(dir) = std::env::var_os("JANUS_TESSERA_REF").map(PathBuf::from) else { return };
        let job: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("job.json")).unwrap()).unwrap();
        let p = &job["params"];
        let b: Vec<f64> = p["bounds"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
        let years: Vec<i32> = p["years"].as_array().unwrap().iter().map(|v| v.as_i64().unwrap() as i32).collect();
        let out = std::env::temp_dir().join("janus-embed-tessera-test");
        let _ = std::fs::remove_dir_all(&out);
        let spec = DownloadSpec {
            source: Source::Tessera,
            years,
            bounds: [b[0], b[1], b[2], b[3]],
            res: p["res"].as_f64(),
            out_dir: out.clone(),
            cache_dir: std::env::temp_dir(),
        };
        let t = Instant::now();
        let files = download(&spec, &|f, m| println!("{:3.0}% {m}", f * 100.0), &AtomicBool::new(false)).unwrap();
        println!("{} file(s) in {:.1} s", files.len(), t.elapsed().as_secs_f64());
        if let Some(keep) = std::env::var_os("JANUS_TESSERA_KEEP") {
            for f in &files {
                let _ = std::fs::copy(f, PathBuf::from(&keep).join(f.file_name().unwrap()));
            }
        }
        for f in &files {
            let name = f.file_name().unwrap();
            let (a, b) = (Dataset::open(f).unwrap(), Dataset::open(dir.join(name)).unwrap());
            assert_eq!(a.raster_size(), b.raster_size(), "{name:?}");
            assert_eq!(a.geo_transform().unwrap(), b.geo_transform().unwrap());
            assert_eq!(a.spatial_ref().unwrap().auth_code().ok(), b.spatial_ref().unwrap().auth_code().ok());
            let (w, h) = a.raster_size();
            let mut differ = 0;
            for band in 1..=a.raster_count() {
                let ra = a.rasterband(band).unwrap().read_as::<i16>((0, 0), (w, h), (w, h), None).unwrap();
                let rb = b.rasterband(band).unwrap().read_as::<i16>((0, 0), (w, h), (w, h), None).unwrap();
                differ += ra.data().iter().zip(rb.data()).filter(|(x, y)| x != y).count();
            }
            println!("{name:?}: {w} x {h} x {}, {differ} values differ", a.raster_count());
            assert_eq!(differ, 0);
        }
        let _ = std::fs::remove_dir_all(&out);
    }

    #[test]
    fn dequantise_and_average() {
        assert!(dequantise(-128).is_nan());
        assert_eq!(dequantise(0), 0.0);
        assert!((dequantise(127) - (127.0f32 / 127.5).powi(2)).abs() < 1e-7);
        assert!((dequantise(-64) + (64.0f32 / 127.5).powi(2)).abs() < 1e-7);
        // 1 band, 4 x 4 cells to 2 x 2 (a NaN left out)
        let c = [1.0, 3.0, 5.0, 5.0, 1.0, f32::NAN, 5.0, 5.0, 0.0, 0.0, 2.0, 4.0, 0.0, 0.0, 6.0, 8.0];
        assert_eq!(average(&c, 1, 4, 4, 2), vec![5.0 / 3.0, 5.0, 0.0, 5.0]);
    }
}
