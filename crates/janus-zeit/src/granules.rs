//! Each date's Sentinel-2 granule metadata (the tile's `MTD_TL.xml`) and the angles read from it: where the files
//! of a series' dates are (`of_series`), reading and parsing them once (`Angles`), and the angles of any window of
//! the series' grid (`Angles::block`), for NBAR with `angles = "sentinel2"` (the pure computation is `s2angles`).
//!
//! A date's metadata comes from the STAC items its VRT records (`STAC_GRANULE_METADATA`, the hrefs of the items'
//! `granule_metadata` or `granule-metadata` assets, kept as the items give them and made readable by
//! `janus_cube::stac::granule_path` when they are read), or, for local files, from the Sentinel-2 product the file is
//! in (`.../X.SAFE/GRANULE/<granule>/IMG_DATA/...` has `.../GRANULE/<granule>/MTD_TL.xml`) or an `MTD_TL.xml` beside
//! the file.

use std::collections::HashMap;
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use gdal::Metadata;
use janus_cube::CubeInfo;
use janus_cube::stac::{GRANULE_METADATA, granule_path};
use rayon::prelude::*;

use crate::s2angles::{self, Like, Prepared};

/// The name of a tile's metadata file.
pub const FILE: &str = "MTD_TL.xml";

/// Whether a run of `tool` with these parameters reads the granule metadata.
pub fn needs(tool: &str, p: &crate::pixel::Params) -> bool {
    tool == "nbar" && p.str("angles") == "sentinel2"
}

/// The metadata of the Sentinel-2 product a local file is in, or beside it.
fn local(path: &str) -> Option<PathBuf> {
    let p = Path::new(path);
    if let Some(m) = p.parent().map(|d| d.join(FILE)).filter(|m| m.is_file()) {
        return Some(m);
    }
    let parts: Vec<_> = p.components().collect();
    parts.iter().enumerate().find_map(|(i, c)| {
        if !c.as_os_str().eq_ignore_ascii_case("GRANULE") || i + 1 >= parts.len() {
            return None;
        }
        // the granule's folder: GRANULE and the folder after it
        let root: PathBuf = parts[..i + 2].iter().collect();
        Some(root.join(FILE)).filter(|m| m.is_file())
    })
}

/// The granule metadata of every date of a series: the hrefs its VRTs record, else the file of the Sentinel-2 product
/// each date's file is in (several for a date made of several tiles; empty where there is none).
pub fn of_series(info: &CubeInfo) -> Vec<Vec<String>> {
    (0..info.t())
        .map(|t| {
            let Some(layer) = info.layers.get(t) else { return vec![] };
            let recorded: Vec<String> = gdal::Dataset::open(&layer.path)
                .ok()
                .and_then(|ds| ds.metadata_item(GRANULE_METADATA, ""))
                .map(|s| s.split_whitespace().map(String::from).collect())
                .unwrap_or_default();
            if !recorded.is_empty() {
                return recorded;
            }
            // a file holding every date (a stack) says nothing of each
            if info.stack || janus_cube::is_remote_path(&layer.path) {
                return vec![];
            }
            local(&layer.path).map(|m| vec![m.to_string_lossy().into_owned()]).unwrap_or_default()
        })
        .collect()
}

/// A file or URL read whole through GDAL's virtual file systems (`href` as the items give it).
pub fn read_text(href: &str) -> Result<String, String> {
    let path = CString::new(granule_path(href)).map_err(|e| e.to_string())?;
    let mode = CString::new("rb").expect("no NUL");
    let mut out = Vec::new();
    // SAFETY: GDAL's file API on a handle opened here and closed before returning.
    unsafe {
        let f = gdal_sys::VSIFOpenL(path.as_ptr(), mode.as_ptr());
        if f.is_null() {
            return Err(format!("{href}: cannot be opened"));
        }
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
    String::from_utf8(out).map_err(|_| format!("{href}: not text"))
}

/// The parsed metadata with the view band's grids fused, by file (a local file's modification time in the key) and
/// band: a pixel run reads the same files again and again.
type Cache = Mutex<HashMap<(String, i32), Arc<Prepared>>>;
static CACHE: LazyLock<Cache> = LazyLock::new(Default::default);

fn load(href: &str, band: i32) -> Result<Arc<Prepared>, String> {
    let stamp = std::fs::metadata(href)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(String::new(), |d| format!("@{}", d.as_nanos()));
    let key = (format!("{href}{stamp}"), band);
    if let Some(p) = CACHE.lock().map_err(|e| e.to_string())?.get(&key) {
        return Ok(p.clone());
    }
    let tile = Arc::new(s2angles::read(&read_text(href)?, href, band)?);
    CACHE.lock().map_err(|e| e.to_string())?.insert(key, tile.clone());
    Ok(tile)
}

/// A date as messages name it.
fn date_text(days: Option<&[i64]>, t: usize) -> String {
    match days.and_then(|d| d.get(t)) {
        Some(&d) => {
            let (y, m, dd) = crate::numeric::civil(d);
            format!("{y:04}-{m:02}-{dd:02}")
        }
        None => format!("date {}", t + 1),
    }
}

/// The angles of a series' dates: each date's tiles read and fused, the grid they are computed on.
pub struct Angles {
    dates: Vec<Vec<Arc<Prepared>>>,
    like: Like,
}

impl Angles {
    /// `granules` are the metadata files of each date (`of_series`), `days` the dates (days since 1970-01-01, for the
    /// check that a file was sensed within a day of its date and for messages), `view_band` the band whose view
    /// angles every band takes ("B06": HLS's), `crs_wkt` and `geotransform` the series' grid. Every file is read and
    /// parsed once; a date without metadata is an error naming it.
    pub fn new(
        granules: &[Vec<String>],
        days: Option<&[i64]>,
        dates: usize,
        view_band: &str,
        crs_wkt: &str,
        geotransform: [f64; 6],
    ) -> Result<Angles, String> {
        let band = s2angles::band_id(view_band)
            .ok_or_else(|| format!("{view_band:?} is not a Sentinel-2 band (B01-B12 or B8A)"))?;
        let none: Vec<String> =
            (0..dates).filter(|&t| granules.get(t).is_none_or(Vec::is_empty)).map(|t| date_text(days, t)).collect();
        if !none.is_empty() {
            let shown: Vec<&str> = none.iter().take(5).map(String::as_str).collect();
            let more = if none.len() > shown.len() {
                format!(" and {} more", none.len() - shown.len())
            } else {
                String::new()
            };
            return Err(format!(
                "no Sentinel-2 granule metadata (MTD_TL.xml) for {}{more}: the dates must be Sentinel-2 files in their \
                 .SAFE product (or with a MTD_TL.xml beside them), or STAC items with a granule metadata asset",
                shown.join(", ")
            ));
        }
        let like = Like::new(crs_wkt, geotransform)?;
        // each distinct file once, on the pool's threads (over a network, the reading is the cost)
        let mut paths: Vec<&String> = granules.iter().take(dates).flatten().collect();
        paths.sort();
        paths.dedup();
        let read: Vec<(&String, Result<Arc<Prepared>, String>)> =
            paths.par_iter().map(|p| (*p, load(p, band))).collect();
        let tiles: HashMap<&String, Arc<Prepared>> =
            read.into_iter().map(|(p, r)| r.map(|t| (p, t))).collect::<Result<_, _>>()?;
        let mut out = Vec::with_capacity(dates);
        for (t, paths) in granules.iter().take(dates).enumerate() {
            let tiles: Vec<Arc<Prepared>> = paths.iter().map(|p| tiles[p].clone()).collect();
            if let Some(&d) = days.and_then(|d| d.get(t)) {
                for tile in &tiles {
                    tile.check_date(d).map_err(|e| format!("{}: {e}", date_text(days, t)))?;
                }
            }
            out.push(tiles);
        }
        Ok(Angles { dates: out, like })
    }

    /// The number of dates.
    pub fn dates(&self) -> usize {
        self.dates.len()
    }

    /// A date's angles (SZA, SAA, VZA, VAA in degrees, row by row) over the window `[x0, y0, w, h]` of the grid, NaN
    /// where its tiles have none.
    pub fn block(&self, t: usize, window: [usize; 4]) -> Result<[Vec<f32>; 4], String> {
        let tiles: Vec<&Prepared> = self.dates[t].iter().map(|p| p.as_ref()).collect();
        s2angles::angles(&tiles, &self.like, window)
    }
}

impl Angles {
    /// The angles of every date over a window as the four bands `sza`, `saa`, `vza`, `vaa` of a chunk ([T][pixels],
    /// float64, degrees), the dates on the pool's threads.
    pub fn planes(&self, window: [usize; 4]) -> Result<Vec<(&'static str, Vec<f64>)>, String> {
        let blocks: Vec<Result<[Vec<f32>; 4], String>> =
            (0..self.dates()).into_par_iter().map(|t| self.block(t, window)).collect();
        let mut planes: Vec<Vec<f64>> =
            (0..4).map(|_| Vec::with_capacity(self.dates() * window[2] * window[3])).collect();
        for block in blocks {
            for (plane, a) in planes.iter_mut().zip(block?) {
                plane.extend(a.iter().map(|&v| f64::from(v)));
            }
        }
        Ok(crate::nbar::ANGLES.iter().copied().zip(planes).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "x").unwrap();
    }

    /// Where a file's metadata is: in its SAFE product's granule folder, or beside it.
    #[test]
    fn local_files_find_their_metadata() {
        let base = std::env::temp_dir().join("janus-zeit-test-granules-local");
        let _ = std::fs::remove_dir_all(&base);
        let safe = base.join("S2B_MSIL2A_20230723T132229_N0509_R038_T23LKC_20230723T150000.SAFE");
        let g = safe.join("GRANULE").join("L2A_T23LKC_A033333_20230723T132229");
        touch(&g.join(FILE));
        touch(&g.join("IMG_DATA").join("R10m").join("T23LKC_20230723T132229_B04_10m.jp2"));
        let band = g.join("IMG_DATA").join("R10m").join("T23LKC_20230723T132229_B04_10m.jp2");
        assert_eq!(local(&band.to_string_lossy()), Some(g.join(FILE)));
        // a product without its metadata, a file of nowhere
        let bare = base.join("X.SAFE").join("GRANULE").join("g").join("IMG_DATA").join("b.jp2");
        touch(&bare);
        assert_eq!(local(&bare.to_string_lossy()), None);
        // a file with its MTD_TL.xml beside it
        let beside = base.join("plain").join("20230723.tif");
        touch(&beside);
        assert_eq!(local(&beside.to_string_lossy()), None);
        touch(&base.join("plain").join(FILE));
        assert_eq!(local(&beside.to_string_lossy()), Some(base.join("plain").join(FILE)));
        let _ = std::fs::remove_dir_all(&base);
    }
}
