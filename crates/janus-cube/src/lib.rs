//! A series of rasters as a cube: its layers (one per date), their dates and how to read them (src/cube.cpp).
//!
//! Phase 1 of RUST_MIGRATION.md opens what the C++ opens most: a folder, pattern or list of files with one date
//! each (band 1 of each), and a single multiband file with one band per date. Remote files, NetCDF/Zarr
//! variables, spectra, embeddings and the band selection (normalized difference, quality band) come in phase 2.

pub mod dates;
pub mod inputs;
mod reader;

use std::sync::atomic::{AtomicU64, Ordering};

use gdal::{Dataset, DatasetOptions, GdalOpenFlags, Metadata};

pub use dates::TimeGranularity;
pub use reader::{CubeReader, with_thread_reader};

/// Nodata and scale/offset of one source band.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BandMeta {
    pub no_data: Option<f64>,
    pub scale: f64,
    pub offset: f64,
}

impl Default for BandMeta {
    fn default() -> Self {
        BandMeta { no_data: None, scale: 1.0, offset: 0.0 }
    }
}

/// One layer of the cube, one date: a file, or a band of a multiband file.
#[derive(Clone, Debug, Default)]
pub struct Layer {
    pub path: String,
    /// the band read (1-based)
    pub band: i32,
    /// Unix seconds (if the cube's time is a date) or the index
    pub time: f64,
    /// the date's text (or the band's or file's name)
    pub label: String,
    pub meta: BandMeta,
}

#[derive(Clone, Debug, Default)]
pub struct CubeInfo {
    /// unique per open (a thread's reader is made again for another cube)
    pub id: u64,
    pub layers: Vec<Layer>,
    pub width: usize,
    pub height: usize,
    pub data_type: String,
    /// GDAL driver of the first layer (e.g. GTiff)
    pub driver: String,
    pub crs_name: String,
    pub crs_authority: String,
    pub geo_transform: Option<[f64; 6]>,
    pub time_is_date: bool,
    pub granularity: TimeGranularity,
    /// e.g. "41 files (band 1 of each)"
    pub description: String,
}

impl CubeInfo {
    pub fn t(&self) -> usize {
        self.layers.len()
    }

    /// The text of a time on the cube's axis: a date at the series' granularity, or the index from 1.
    pub fn format_time(&self, t: f64) -> String {
        if self.time_is_date { dates::format_date(t, self.granularity) } else { format!("{}", t.round() as i64 + 1) }
    }

    /// Decimal year of layer t (e.g. 2003.5); the index when there are no dates.
    pub fn decimal_year(&self, t: usize) -> f64 {
        if self.time_is_date { dates::decimal_year(self.layers[t].time) } else { t as f64 }
    }

    /// Time of layer t in decimal years from the first layer (for trends).
    pub fn years_from_start(&self, t: usize) -> f64 {
        if self.time_is_date { (self.layers[t].time - self.layers[0].time) / (365.2425 * 86400.0) } else { t as f64 }
    }

    /// The layer whose time is nearest to `time`.
    pub fn nearest_layer(&self, time: f64) -> usize {
        (0..self.t())
            .min_by(|&a, &b| (self.layers[a].time - time).abs().total_cmp(&(self.layers[b].time - time).abs()))
            .unwrap_or(0)
    }
}

/// Opens a raster read-only.
pub fn open_raster(path: &str) -> gdal::errors::Result<Dataset> {
    Dataset::open_ex(
        path,
        DatasetOptions {
            open_flags: GdalOpenFlags::GDAL_OF_RASTER | GdalOpenFlags::GDAL_OF_READONLY,
            ..Default::default()
        },
    )
}

fn band_meta(b: &gdal::raster::RasterBand) -> BandMeta {
    BandMeta { no_data: b.no_data_value(), scale: b.scale().unwrap_or(1.0), offset: b.offset().unwrap_or(0.0) }
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Interprets the inputs: a folder, a wildcard pattern, a list of files (one date each, band 1), or a single file
/// whose bands are the dates.
pub fn open_cube(inputs: &[String]) -> Result<CubeInfo, String> {
    let files = inputs::expand_inputs(inputs);
    if files.is_empty() {
        return Err("No raster files found.".into());
    }
    let open = |f: &str| open_raster(f).map_err(|e| format!("{f}: {e}"));
    let first = open(&files[0])?;
    let (width, height) = first.raster_size();
    if first.raster_count() == 0 {
        return Err(format!("{}: no raster bands.", files[0]));
    }
    let mut info = CubeInfo {
        id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        width,
        height,
        data_type: first.rasterband(1).map(|b| b.band_type().name()).unwrap_or_default(),
        driver: first.driver().short_name(),
        geo_transform: first.geo_transform().ok(),
        ..Default::default()
    };
    if let Ok(srs) = first.spatial_ref() {
        info.crs_name = srs.name().unwrap_or_default();
        if let (Some(a), Ok(c)) = (srs.auth_name(), srs.auth_code()) {
            info.crs_authority = format!("{a}:{c}");
        }
    }

    // where each layer's date is looked for
    let mut time_texts = Vec::new();
    if files.len() == 1 {
        // one file: each band is one date
        let n = first.raster_count();
        for b in 1..=n {
            let rb = first.rasterband(b).map_err(|e| e.to_string())?;
            let desc = rb.description().unwrap_or_default();
            let mut text = desc.clone();
            for item in rb.metadata_domain("").unwrap_or_default() {
                text.push(' ');
                text.push_str(&item);
            }
            time_texts.push(text);
            info.layers.push(Layer {
                path: files[0].clone(),
                band: b as i32,
                label: if desc.is_empty() { format!("band {b}") } else { desc },
                meta: band_meta(&rb),
                ..Default::default()
            });
        }
        info.description = format!("1 file, {n} bands (1 band = 1 date)");
    } else {
        drop(first);
        for f in &files {
            let ds = open(f)?;
            if ds.raster_size() != (width, height) {
                let (w, h) = ds.raster_size();
                return Err(format!(
                    "{}: size {w}x{h} differs from the 1st file ({width}x{height}).",
                    inputs::display_name(f)
                ));
            }
            let rb = ds.rasterband(1).map_err(|e| format!("{f}: {e}"))?;
            let label = inputs::display_name(f);
            time_texts.push(label.clone());
            info.layers.push(Layer { path: f.clone(), band: 1, label, meta: band_meta(&rb), ..Default::default() });
        }
        info.description = format!("{} files (band 1 of each)", files.len());
    }

    // Dates: only used if every layer has one. Files in a folder each, with the date on the folder (Sentinel-2
    // COGs on AWS: .../S2B_23KPQ_20230811_0_L2A/B04.tif), take their folder's.
    let folders: Vec<String> = info
        .layers
        .iter()
        .map(|l| {
            let f = l.path.split('?').next().unwrap_or("");
            let parts: Vec<&str> = f.split(['/', '\\']).collect();
            if parts.len() >= 2 { parts[parts.len() - 2].to_string() } else { String::new() }
        })
        .collect();
    let mut distinct = folders.clone();
    distinct.sort();
    distinct.dedup();
    let folder_per_file = files.len() > 1 && distinct.len() == folders.len();
    let mut found = Vec::with_capacity(info.t());
    for (i, text) in time_texts.iter().enumerate() {
        let d = dates::parse_date(text).or_else(|| folder_per_file.then(|| dates::parse_date(&folders[i])).flatten());
        match d {
            Some(d) => found.push(d),
            None => break,
        }
    }
    if found.len() == info.t() {
        for (l, (t, _)) in info.layers.iter_mut().zip(&found) {
            l.time = *t;
        }
        info.layers.sort_by(|a, b| a.time.total_cmp(&b.time));
        info.time_is_date = true;
        info.granularity = found.iter().map(|d| d.1).fold(TimeGranularity::Year, Ord::max);
        let g = info.granularity;
        for l in &mut info.layers {
            l.label = dates::format_date(l.time, g);
        }
    } else {
        for (i, l) in info.layers.iter_mut().enumerate() {
            l.time = i as f64;
        }
    }
    Ok(info)
}

/// The version of the GDAL in use.
pub fn gdal_version() -> String {
    gdal::version::version_info("RELEASE_NAME")
}
