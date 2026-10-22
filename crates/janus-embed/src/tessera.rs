//! TESSERA (geotessera.org) read natively, as zeit/_tessera.py reads it: the published Zarr v3 store holds a group
//! per UTM zone (utm01 ... utm60) on the grid the embeddings were made on, with `embeddings` (time, band, y, x) int8
//! and `scales` (time, y, x) float32, one scale per pixel (a non-finite scale marks a pixel without an embedding),
//! the zone's CRS and transform in its attributes. Both arrays are sharded (shards of 4096 x 4096 cells, inner
//! chunks of 32 x 32 compressed with blosc, the shard's index at its end); a pixel's value is its int8 times its
//! scale. South of the equator the store keeps the northern zone's CRS with negative northings: a region wholly
//! south of it is given in the southern zone's CRS, the same cells (zeit's choice).
//!
//! The shards are read by ranges over HTTP (GDAL's /vsicurl/, the program's libcurl) and decompressed by GDAL's
//! blosc: GDAL's own Zarr driver does not read sharded arrays.

use std::collections::HashMap;
use std::ffi::CString;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use serde_json::Value;

use crate::download::{DownloadSpec, GeoTiff, bounds_in, http_options};

/// The store of TESSERA v1.1 (geotessera's default, the "dclimate" run), at the bucket's own endpoint rather than
/// the data.source.coop gateway (as AlphaEarth's).
const STORE: &str =
    "https://s3.us-west-2.amazonaws.com/us-west-2.opendata.source.coop/tessera/tessera/zarr/v1.1-dclimate";
const VERSION: &str = "1.1";
const VARIANT: &str = "dclimate";
/// the range of the dequantised values kept in Int16 (task_embeddings.py's: they reach about +-18)
const RANGE: f64 = 32.0;
/// rows of the output a strip (zeit's block side)
const STRIP: usize = 512;
/// false northing of the southern UTM zones
const SOUTH: f64 = 10_000_000.0;

/// A file of the store read whole (None: absent).
fn read_all(url: &str) -> Result<Option<Vec<u8>>, String> {
    let src = CString::new(format!("/vsicurl/{url}")).map_err(|e| e.to_string())?;
    let mode = CString::new("rb").unwrap();
    let mut out = Vec::new();
    // SAFETY: GDAL's file API on a handle opened here and closed before returning.
    unsafe {
        let f = gdal_sys::VSIFOpenL(src.as_ptr(), mode.as_ptr());
        if f.is_null() {
            return Ok(None);
        }
        // to its end (a size from a HEAD request can be missing)
        let mut buf = vec![0u8; 1 << 16];
        loop {
            let n = gdal_sys::VSIFReadL(buf.as_mut_ptr().cast(), 1, buf.len(), f);
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
        gdal_sys::VSIFCloseL(f);
    }
    Ok(Some(out))
}

/// A file's size (None: absent), from its end.
fn size_of(url: &str) -> Option<u64> {
    let src = CString::new(format!("/vsicurl/{url}")).ok()?;
    let mode = CString::new("rb").unwrap();
    // SAFETY: GDAL's file API on a handle opened here and closed before returning.
    unsafe {
        let f = gdal_sys::VSIFOpenL(src.as_ptr(), mode.as_ptr());
        if f.is_null() {
            return None;
        }
        let size = (gdal_sys::VSIFSeekL(f, 0, 2) == 0).then(|| gdal_sys::VSIFTellL(f));
        gdal_sys::VSIFCloseL(f);
        size
    }
}

/// `len` bytes of a file of the store from `offset`.
fn read_range(url: &str, offset: u64, len: usize) -> Result<Vec<u8>, String> {
    let src = CString::new(format!("/vsicurl/{url}")).map_err(|e| e.to_string())?;
    let mode = CString::new("rb").unwrap();
    let mut buf = vec![0u8; len];
    // SAFETY: GDAL's file API on a handle opened here and closed before returning; the buffer holds `len` bytes.
    unsafe {
        let f = gdal_sys::VSIFOpenL(src.as_ptr(), mode.as_ptr());
        if f.is_null() {
            return Err(format!("could not reach {url}"));
        }
        let ok =
            gdal_sys::VSIFSeekL(f, offset, 0) == 0 && gdal_sys::VSIFReadL(buf.as_mut_ptr().cast(), 1, len, f) == len;
        gdal_sys::VSIFCloseL(f);
        if !ok {
            return Err(format!("{url}: could not read {len} bytes at {offset}"));
        }
    }
    Ok(buf)
}

fn json(url: &str) -> Result<Option<Value>, String> {
    match read_all(url)? {
        Some(b) => serde_json::from_slice(&b).map(Some).map_err(|e| format!("{url}: {e}")),
        None => Ok(None),
    }
}

/// The shards' indexes read so far, by URL (None: the shard is absent).
type Indexes = Mutex<HashMap<String, Option<Vec<(u64, u64)>>>>;

/// An array's layout: its shape, the shard's and the inner chunk's (both over every dimension).
struct Sharded {
    url: String,
    shape: Vec<usize>,
    shard: Vec<usize>,
    inner: Vec<usize>,
    /// the inner chunk's bytes once decompressed
    chunk_bytes: usize,
}

impl Sharded {
    fn open(url: &str, item: usize) -> Result<Sharded, String> {
        let m = json(&format!("{url}/zarr.json"))?.ok_or_else(|| format!("{url}: no zarr.json"))?;
        let dims = |v: &Value| -> Vec<usize> {
            v.as_array().map(|a| a.iter().filter_map(Value::as_u64).map(|x| x as usize).collect()).unwrap_or_default()
        };
        let shape = dims(&m["shape"]);
        let shard = dims(&m["chunk_grid"]["configuration"]["chunk_shape"]);
        let codec = &m["codecs"][0];
        let cfg = &codec["configuration"];
        let inner = dims(&cfg["chunk_shape"]);
        let names: Vec<&str> =
            cfg["codecs"].as_array().map(|a| a.iter().filter_map(|c| c["name"].as_str()).collect()).unwrap_or_default();
        if codec["name"] != "sharding_indexed"
            || cfg["index_location"] != "end"
            || names != ["bytes", "blosc"]
            || shape.len() != shard.len()
            || shape.len() != inner.len()
        {
            return Err(format!("{url}: a layout this reader does not know ({codec})"));
        }
        let chunk_bytes = inner.iter().product::<usize>() * item;
        Ok(Sharded { url: url.to_string(), shape, shard, inner, chunk_bytes })
    }

    /// Inner chunks a shard holds.
    fn per_shard(&self) -> usize {
        self.shard.iter().zip(&self.inner).map(|(s, i)| s / i).product()
    }

    /// A shard's index: (offset, bytes) of each inner chunk in C order; None: the shard is absent.
    fn index(&self, key: &str, cache: &Indexes) -> Result<Option<Vec<(u64, u64)>>, String> {
        let url = format!("{}/c/{key}", self.url);
        if let Some(i) = cache.lock().unwrap().get(&url) {
            return Ok(i.clone());
        }
        let n = self.per_shard();
        let index = match size_of(&url) {
            None => None,
            Some(size) => {
                let len = n * 16 + 4; // pairs of little-endian u64, then the CRC32C
                if (size as usize) < len {
                    return Err(format!("{url}: shorter than its index"));
                }
                let b = read_range(&url, size - len as u64, len)?;
                let u = |k: usize| u64::from_le_bytes(b[k * 8..k * 8 + 8].try_into().unwrap());
                Some((0..n).map(|k| (u(2 * k), u(2 * k + 1))).collect())
            }
        };
        cache.lock().unwrap().insert(url, index.clone());
        Ok(index)
    }

    /// An inner chunk decompressed (None: not written, the fill value).
    fn chunk(&self, key: &str, at: (u64, u64), blosc: &janus_gdal::Codec) -> Result<Option<Vec<u8>>, String> {
        if at.0 == u64::MAX && at.1 == u64::MAX {
            return Ok(None);
        }
        let url = format!("{}/c/{key}", self.url);
        let raw = read_range(&url, at.0, at.1 as usize)?;
        let mut out = vec![0u8; self.chunk_bytes];
        if !blosc.decompress(&raw, &mut out) {
            return Err(format!("{url}: a chunk blosc could not decompress"));
        }
        Ok(Some(out))
    }
}

/// The zones a longitude/latitude box touches (zeit/_tessera.py, _zones).
fn zones(bbox: [f64; 4]) -> Vec<i32> {
    let [w, _, e, _] = bbox;
    let first = (((w + 180.0) / 6.0).floor() as i32 + 1).min(60);
    let last = (((e - 1e-9 + 180.0) / 6.0).floor() as i32 + 1).min(60);
    (first..=last).collect()
}

/// The meta of the store (zeit's tags_of), the GeoTIFFs' ZEIT_EMBEDDING tag.
fn tag(root: &Value, dims: usize) -> String {
    let a = &root["attributes"];
    serde_json::json!({
        "embedding_source": "tessera",
        "embedding_version": VERSION,
        "embedding_variant": VARIANT,
        "embedding_model": a["geoemb:model"].as_str().unwrap_or(""),
        "embedding_build": a["geoemb:build_version"].as_str().unwrap_or(""),
        "embedding_dimensions": dims,
        "embedding_license": "CC0-1.0",
        "embedding_citation": "Feng et al. (2025), TESSERA: Temporal Embeddings of Surface Spectra for Earth \
                               Representation and Analysis, arXiv:2506.20380",
    })
    .to_string()
}

/// TESSERA over the region: one GeoTIFF per year (the same files as task_embeddings.py's through Zeit).
pub(crate) fn download(
    spec: &DownloadSpec,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<Vec<std::path::PathBuf>, String> {
    http_options(); // which lets /vsicurl/ read the store's zarr.json and its keys without an extension
    let blosc = janus_gdal::Codec::get("blosc").ok_or("this GDAL has no blosc (TESSERA's compression)")?;
    progress(0.0, "finding the data");
    let zs = zones(spec.bounds);
    if zs.len() > 1 {
        return Err(format!(
            "the region spans {} UTM zones ({}): download each side on its own",
            zs.len(),
            zs.iter().map(|z| format!("{z}")).collect::<Vec<_>>().join(", ")
        ));
    }
    let group = format!("{STORE}/utm{:02}", zs[0]);
    let none = || {
        let [w, s, e, n] = spec.bounds;
        format!("no TESSERA embeddings cover the region ({w:.5}, {s:.5}, {e:.5}, {n:.5})")
    };
    let g = json(&format!("{group}/zarr.json"))?.ok_or_else(none)?;
    let root = json(&format!("{STORE}/zarr.json"))?.unwrap_or(Value::Null);
    let attrs = &g["attributes"];
    let crs = attrs["proj:code"].as_str().map(str::to_string).unwrap_or_else(|| format!("EPSG:{}", 32600 + zs[0]));
    let t: Vec<f64> =
        attrs["spatial:transform"].as_array().map(|a| a.iter().filter_map(Value::as_f64).collect()).unwrap_or_default();
    if t.len() < 6 {
        return Err(format!("{group}: no transform"));
    }
    let (a, c, e, f) = (t[0], t[2], t[4], t[5]);
    let emb = Sharded::open(&format!("{group}/embeddings"), 1)?;
    let sc = Sharded::open(&format!("{group}/scales"), 4)?;
    let dims = emb.shape[1];
    // the years the zone has
    let years: Vec<i32> = match read_all(&format!("{group}/time/c/0"))? {
        Some(raw) => {
            let n =
                json(&format!("{group}/time/zarr.json"))?.and_then(|m| m["shape"][0].as_u64()).unwrap_or(0) as usize;
            let mut out = vec![0u8; n * 4];
            if !blosc.decompress(&raw, &mut out) {
                return Err(format!("{group}/time: blosc could not decompress it"));
            }
            out.as_chunks::<4>().0.iter().map(|b| i32::from_le_bytes(*b)).collect()
        }
        None => Vec::new(),
    };
    let missing: Vec<i32> = spec.years.iter().copied().filter(|y| !years.contains(y)).collect();
    if !missing.is_empty() {
        return Err(format!("TESSERA has the years {years:?}, not {missing:?}"));
    }
    // the zone's cells over the region (zeit's window_of)
    let (zh, zw) = (emb.shape[2] as i64, emb.shape[3] as i64);
    let b = bounds_in(spec.bounds, &crs)?;
    let col0 = (((b[0] - c) / a + 1e-6).floor() as i64).max(0);
    let col1 = (((b[2] - c) / a - 1e-6).ceil() as i64).min(zw);
    let row0 = (((b[3] - f) / e + 1e-6).floor() as i64).max(0);
    let row1 = (((b[1] - f) / e - 1e-6).ceil() as i64).min(zh);
    if col1 <= col0 || row1 <= row0 {
        return Err(none());
    }
    // a region wholly south of the equator: the southern zone's CRS, the same cells
    let epsg: i32 = crs.trim_start_matches("EPSG:").parse().unwrap_or(0);
    let (out_crs, shift) = if spec.bounds[3] <= 0.0 && (32601..=32660).contains(&epsg) {
        (format!("EPSG:{}", epsg + 100), SOUTH)
    } else {
        (crs.clone(), 0.0)
    };
    // the output grid: the window's cells, or cells of `res` on multiples of it (the mean of the 10 m cells under
    // each), as AlphaEarth's
    let res = spec.res.filter(|&r| r > a).unwrap_or(a);
    let k = (res / a).round().max(1.0) as usize;
    let (x0, y0, ow, oh) = if k == 1 {
        (c + a * col0 as f64, f + shift + e * row0 as f64, (col1 - col0) as usize, (row1 - row0) as usize)
    } else {
        let bo = bounds_in(spec.bounds, &out_crs)?;
        let x0 = (bo[0] / res + 1e-6).floor() * res;
        let y0 = (bo[3] / res - 1e-6).ceil() * res;
        let ow = (((bo[2] - x0) / res - 1e-6).ceil() as usize).max(1);
        let oh = (((y0 - bo[1]) / res - 1e-6).ceil() as usize).max(1);
        (x0, y0, ow, oh)
    };
    // the zone's cell of the output's first one
    let (zc0, zr0) = (((x0 - c) / a).round() as i64, ((f + shift - y0) / -e).round() as i64);
    // a coarser grid: the window's cells warped onto it by zeit's kernel (the average by area, GDAL's scales)
    let src_gt = [c + a * col0 as f64, a, 0.0, f + shift + e * row0 as f64, 0.0, e];
    let (win_h, win_w) = ((row1 - row0) as usize, (col1 - col0) as usize);
    let coarse = (k > 1).then(|| Coarse::new(src_gt, win_h, win_w, [x0, res, 0.0, y0, 0.0, -res], oh, ow));
    let tag = tag(&root, dims);
    let scale = RANGE / 32767.0;
    let strip_rows = (STRIP / k).max(1);
    let strips: Vec<(usize, usize)> = (0..oh).step_by(strip_rows).map(|r| (r, strip_rows.min(oh - r))).collect();
    let total = spec.years.len() * strips.len();
    let done = AtomicUsize::new(0);
    let t0 = Instant::now();
    let indexes = Mutex::new(HashMap::new());
    let mut outputs = Vec::new();
    for (yi, &year) in spec.years.iter().enumerate() {
        let ti = years.iter().position(|&y| y == year).unwrap();
        let path = spec.out_dir.join(format!("tessera_{year}.tif"));
        let mut out = GeoTiff::create(&path, ow, oh, dims, [x0, res, 0.0, y0, 0.0, -res], &out_crs, scale, &tag)?;
        for &(r, h) in &strips {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            let window = [row0, row1, col0, col1];
            let block = match &coarse {
                None => read_cells(&emb, &sc, ti, zr0 + r as i64, h, zc0, ow, window, &blosc, &indexes, cancel)?,
                Some(cw) => match cw.window(r, h) {
                    // the window's cells this strip's samples see, warped as zeit warps them
                    Some([wa, wb, wc, wd]) => {
                        let (src_h, src_w) = (wb - wa, wd - wc);
                        let (cr, cc) = (row0 + wa as i64, col0 + wc as i64);
                        let cells = read_cells(&emb, &sc, ti, cr, src_h, cc, src_w, window, &blosc, &indexes, cancel)?;
                        cw.warp(&cells, dims, [wa, wc, src_h, src_w], r, h)
                    }
                    None => vec![f32::NAN; dims * h * ow],
                },
            };
            out.write_strip(r, h, &block)?;
            let d = done.fetch_add(1, Ordering::Relaxed) + 1;
            let el = t0.elapsed().as_secs_f64();
            progress(
                d as f64 / total as f64,
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

/// The cells (rows r0 .. r0 + h, columns c0 .. c0 + w of the zone's grid) at time step t, [band][row][col] as float:
/// int8 times the pixel's scale, NaN where the scale is not finite or outside `window` (row0, row1, col0, col1: the
/// region's cells, which zeit's cube holds; a coarser output's border cells average only those). The inner chunks
/// are read on several threads (the requests wait more than they compute).
#[allow(clippy::too_many_arguments)]
fn read_cells(
    emb: &Sharded,
    sc: &Sharded,
    t: usize,
    r0: i64,
    h: usize,
    c0: i64,
    w: usize,
    window: [i64; 4],
    blosc: &janus_gdal::Codec,
    indexes: &Indexes,
    cancel: &AtomicBool,
) -> Result<Vec<f32>, String> {
    let dims = emb.shape[1];
    let (ih, iw) = (emb.inner[2] as i64, emb.inner[3] as i64);
    let (sh, sw) = (emb.shard[2] as i64, emb.shard[3] as i64);
    let out = Mutex::new(vec![f32::NAN; dims * h * w]);
    // the inner chunks under the cells, within the window
    let (ra, rb) = (r0.max(window[0]), (r0 + h as i64).min(window[1]));
    let (ca, cb) = (c0.max(window[2]), (c0 + w as i64).min(window[3]));
    let mut tasks = Vec::new();
    if ra < rb && ca < cb {
        for cy in ra / ih..=(rb - 1) / ih {
            for cx in ca / iw..=(cb - 1) / iw {
                tasks.push((cy, cx));
            }
        }
    }
    let first_error: Mutex<Option<String>> = Mutex::new(None);
    let next = AtomicUsize::new(0);
    let per_row = sw / iw; // inner chunks across a shard
    std::thread::scope(|s| {
        for _ in 0..16.min(tasks.len()) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= tasks.len() || cancel.load(Ordering::Relaxed) || first_error.lock().unwrap().is_some() {
                        break;
                    }
                    let (cy, cx) = tasks[i];
                    let r = (|| -> Result<(), String> {
                        let (sy, sx) = (cy * ih / sh, cx * iw / sw);
                        let k = ((cy * ih % sh) / ih * per_row + (cx * iw % sw) / iw) as usize;
                        let ekey = format!("{t}/0/{sy}/{sx}");
                        let skey = format!("{t}/{sy}/{sx}");
                        let (Some(ei), Some(si)) = (emb.index(&ekey, indexes)?, sc.index(&skey, indexes)?) else {
                            return Ok(()); // a shard not written: no embeddings there
                        };
                        let Some(scales) = sc.chunk(&skey, si[k], blosc)? else { return Ok(()) };
                        let values = emb.chunk(&ekey, ei[k], blosc)?; // None: the fill value, 0
                        let scale = |p: usize| f32::from_le_bytes(scales[p * 4..p * 4 + 4].try_into().unwrap());
                        let mut o = out.lock().unwrap();
                        let (y0, x0) = (cy * ih, cx * iw);
                        for yy in y0.max(ra)..(y0 + ih).min(rb) {
                            for xx in x0.max(ca)..(x0 + iw).min(cb) {
                                let p = ((yy - y0) * iw + (xx - x0)) as usize;
                                let s = scale(p);
                                if !s.is_finite() {
                                    continue;
                                }
                                let dst = (yy - r0) as usize * w + (xx - c0) as usize;
                                for band in 0..dims {
                                    let v = values.as_ref().map_or(0i8, |v| v[band * (ih * iw) as usize + p] as i8);
                                    o[band * h * w + dst] = f32::from(v) * s;
                                }
                            }
                        }
                        Ok(())
                    })();
                    if let Err(e) = r {
                        *first_error.lock().unwrap() = Some(e);
                        break;
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

/// zeit's warp of a TESSERA window onto a coarser grid of the same CRS (zeit/_warp.py, the affine case of _Warp):
/// GDAL's dfXScale / dfYScale from the source window the destination's edges see, the average by area of
/// zeit-core's kernel, every band on its own with NaN as NoData.
struct Coarse {
    /// the destination's geotransform and the inverse of the source's (GDAL's order)
    gt: [f64; 6],
    inv: [f64; 6],
    src_h: usize,
    src_w: usize,
    dw: usize,
    xscale: f64,
    yscale: f64,
}

impl Coarse {
    fn new(src_gt: [f64; 6], src_h: usize, src_w: usize, dst_gt: [f64; 6], dh: usize, dw: usize) -> Coarse {
        let inv = [-src_gt[0] / src_gt[1], 1.0 / src_gt[1], 0.0, -src_gt[3] / src_gt[5], 0.0, 1.0 / src_gt[5]];
        let mut c = Coarse { gt: dst_gt, inv, src_h, src_w, dw, xscale: 1.0, yscale: 1.0 };
        // GDALWarpOperation::ComputeSourceWindow: points along the destination's edges (GDAL's 21 a side)
        let step = 1.0 / 20.0;
        let (w, h) = (dw as f64, dh as f64);
        let (mut xs, mut ys) = (Vec::new(), Vec::new());
        let mut ratio = 0.0f64;
        while ratio <= 1.0 + step * 0.5 {
            for (u, v) in [(ratio * w, 0.0), (ratio * w, h), (0.0, ratio * h), (w, ratio * h)] {
                let (x, y) = c.point(u, v);
                xs.push(x);
                ys.push(y);
            }
            ratio += step;
        }
        let snap = |v: f64| {
            let r = v.round_ties_even();
            if (r - v).abs() < 1e-6 { r } else { v }
        };
        let min = |a: &[f64]| snap(a.iter().copied().fold(f64::INFINITY, f64::min));
        let max = |a: &[f64]| snap(a.iter().copied().fold(f64::NEG_INFINITY, f64::max));
        c.xscale = kernel_scale(min(&xs), max(&xs), dw, src_w);
        c.yscale = kernel_scale(min(&ys), max(&ys), dh, src_h);
        c
    }

    /// The source pixel coordinates of destination point (u, v).
    fn point(&self, u: f64, v: f64) -> (f64, f64) {
        let (g, i) = (&self.gt, &self.inv);
        let gx = g[0] + u * g[1] + v * g[2];
        let gy = g[3] + u * g[4] + v * g[5];
        (i[0] + gx * i[1] + gy * i[2], i[3] + gx * i[4] + gy * i[5])
    }

    /// The source cells (row0, row1, col0, col1) destination rows r .. r + h need (zeit's _Warp.window, its pad of
    /// 2 for an area method), None if none.
    fn window(&self, r: usize, h: usize) -> Option<[usize; 4]> {
        let (r0, r1, c1) = (r as f64, (r + h) as f64, self.dw as f64);
        let pts = [self.point(0.0, r0), self.point(c1, r0), self.point(0.0, r1), self.point(c1, r1)];
        let (x0, x1) = pts.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), p| (a.min(p.0), b.max(p.0)));
        let (y0, y1) = pts.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), p| (a.min(p.1), b.max(p.1)));
        let a = (y0.floor() as i64 - 2).max(0) as usize;
        let b = ((y1.ceil() as i64 + 2).max(0) as usize).min(self.src_h);
        let c = (x0.floor() as i64 - 2).max(0) as usize;
        let d = ((x1.ceil() as i64 + 2).max(0) as usize).min(self.src_w);
        (b > a && d > c).then_some([a, b, c, d])
    }

    /// Destination rows r .. r + h of every band from `cells` ([band][win h][win w], the source window at
    /// (row, col)), [band][h][dw].
    fn warp(&self, cells: &[f32], bands: usize, win: [usize; 4], r: usize, h: usize) -> Vec<f32> {
        use zeit_core::warp;
        let coords = warp::Coords { kind: warp::AFFINE, offset: 0.0, gt: self.gt, inv: self.inv, ..Default::default() };
        let spec = warp::Spec {
            method: warp::AVERAGE,
            bands: bands as i32,
            src_h: self.src_h as i32,
            src_w: self.src_w as i32,
            win_r0: win[0] as i32,
            win_c0: win[1] as i32,
            win_h: win[2] as i32,
            win_w: win[3] as i32,
            xscale: self.xscale,
            yscale: self.yscale,
            has_src_nodata: true,
            per_band: true,
            src_nodata: f64::NAN,
            dst_nodata: f64::NAN,
            dst_r0: r as i32,
            dst_c0: 0,
            dst_h: h as i32,
            dst_w: self.dw as i32,
        };
        let mut out = vec![f32::NAN; bands * h * self.dw];
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
        warp::warp(cells, &mut out, &coords, &spec, threads);
        out
    }
}

/// GDAL's dfXScale (or dfYScale) of a side (zeit/_warp.py, _kernel_scales' side, an area method): the destination's
/// cells over the source cells its edges span within the source.
fn kernel_scale(lo: f64, hi: f64, n_dst: usize, n_src: usize) -> f64 {
    let n = n_src as f64;
    let lo_c = lo.max(0.0) as i64;
    let hi_c = hi.ceil().min(n) as i64;
    let raw = 0f64.max((n - lo_c as f64).min(hi - lo));
    let size = if (hi_c - lo_c) as f64 > 0.9 * n {
        n
    } else {
        let off = lo_c.clamp(0, n_src as i64);
        ((n_src as i64 - off).min(hi_c - off)).max(0) as f64
    };
    let extra = size - raw;
    let mut scale = n_dst as f64 / (size - extra);
    if size >= n_dst as f64 && size <= n_dst as f64 + extra {
        scale = 1.0;
    }
    if scale < 1.0 {
        let reciprocal = 1.0 / scale;
        let whole = (reciprocal + 0.5) as i64;
        if (reciprocal - whole as f64).abs() < 0.05 {
            scale = 1.0 / whole as f64;
        }
    }
    scale
}
