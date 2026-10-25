//! A series of rasters as a cube (src/cube.cpp): its layers (one per date, or per wavelength of a spectrum),
//! their dates, the band each shows (or the normalized difference of two, with a quality band masking clouds),
//! and how to read them through GDAL. Opens what the C++ opens: a folder, a pattern or a list of files (one date
//! each), a single multiband file (one band per date or per wavelength), a file of embeddings, a variable of a
//! NetCDF / Zarr / HDF file, and the same over the network.

pub mod calc;
pub mod dates;
pub mod inputs;
pub mod multidim;
mod reader;
pub mod remote;
pub mod roles;
pub mod spectral;
pub mod stac;
pub mod stac_vrt;
pub mod stats;
pub mod vrt;

use std::collections::BTreeMap;
use std::ffi::{CStr, CString};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use gdal::{Dataset, DatasetOptions, GdalOpenFlags, Metadata};

pub use dates::TimeGranularity;
pub use reader::{CubeReader, with_thread_reader};
pub use remote::{dataset_file, display_name, gdal_path, is_remote_path};

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

/// How the quality band marks usable observations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum QaRule {
    #[default]
    None,
    /// Fmask codes: 0 clear land, 1 water usable; 2 shadow, 3 snow, 4 cloud, 255 fill masked
    Fmask,
    /// Landsat Collection 2 QA_PIXEL bits: fill, dilated cloud, cloud, shadow, snow masked
    LandsatC2,
    /// a generic mask: 0 = masked, anything else usable
    NonZero,
    /// Sentinel-2's scene classification (SCL): 4 vegetation, 5 bare soil, 6 water usable; no data, saturated,
    /// shadows, clouds, cirrus, snow and unclassified masked
    Scl,
}

impl QaRule {
    /// The C++'s numbers (they enter the caches' keys).
    pub fn code(self) -> i32 {
        self as i32
    }

    pub fn id(self) -> &'static str {
        match self {
            QaRule::Fmask => "fmask",
            QaRule::LandsatC2 => "landsat_c2",
            QaRule::NonZero => "nonzero",
            QaRule::Scl => "scl",
            QaRule::None => "none",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            QaRule::Fmask => "Fmask codes (0 clear, 1 water)",
            QaRule::LandsatC2 => "Landsat C2 QA_PIXEL bits",
            QaRule::NonZero => "Mask (0 = invalid)",
            QaRule::Scl => "Sentinel-2 SCL classes",
            QaRule::None => "None",
        }
    }

    pub fn usable(self, code: u32) -> bool {
        match self {
            QaRule::Fmask => code == 0 || code == 1,
            // bits 0 fill, 1 dilated cloud, 3 cloud, 4 cloud shadow, 5 snow
            QaRule::LandsatC2 => code & 0x3B == 0,
            QaRule::NonZero => code != 0,
            QaRule::Scl => (4..=6).contains(&code),
            QaRule::None => true,
        }
    }
}

/// When every date is a file with several bands (e.g. Landsat surface reflectance + QA): what the series shows.
/// Ignored for a single multiband file, where each band is a date.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BandSelection {
    /// the shown band (1-based)
    pub band: i32,
    /// > 0: show the normalized difference (band - nd_band) / (band + nd_band)
    pub nd_band: i32,
    /// > 0: the quality band; unusable observations become no-data
    pub qa_band: i32,
    pub qa_rule: QaRule,
    /// set by the user: no automatic quality band or index
    pub chosen: bool,
    /// an expression over the date's bands (an index of the library, one typed): shown instead of the band or
    /// normalized difference, before the quality mask
    pub expr: calc::ExprId,
}

impl Default for BandSelection {
    fn default() -> Self {
        BandSelection {
            band: 1,
            nd_band: 0,
            qa_band: 0,
            qa_rule: QaRule::None,
            chosen: false,
            expr: calc::ExprId::NONE,
        }
    }
}

/// One layer of the cube, one date: a file, or a band of a multiband file.
#[derive(Clone, Debug, Default)]
pub struct Layer {
    pub path: String,
    /// the shown band of the file (absolute, 1-based)
    pub band: i32,
    /// the date's bands are band_base + 1 ..= band_base + bands_per_date of the file (0, except embeddings of
    /// several years in one file)
    pub band_base: i32,
    /// Unix seconds (time_is_date), nm (spectral) or the index
    pub time: f64,
    /// the date's text (or the band's or file's name)
    pub label: String,
    pub meta: BandMeta,
    /// of the normalized difference's second band, if any
    pub nd_meta: BandMeta,
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
    /// WKT2 of the CRS ("" = none), for reprojection
    pub crs_wkt: String,
    pub geo_transform: Option<[f64; 6]>,
    pub time_is_date: bool,
    /// one file whose bands are wavelengths (hyperspectral): the axis is the spectrum, layers' time in nm
    pub spectral: bool,
    /// each band's wavelength (nm), in the file's band order, when the bands carry one
    pub band_nm: Vec<f64>,
    pub granularity: TimeGranularity,
    /// e.g. "41 files (band 1 of each)"
    pub description: String,
    pub first_path: String,
    /// one file per date: the bands of each file and what the series shows
    pub bands_per_date: usize,
    pub band_names: Vec<String>,
    pub sel: BandSelection,
    /// categorical data declared by the file (a colour table, category names or an attribute table)
    pub file_categorical: bool,
    pub class_colors: BTreeMap<i32, [u8; 4]>,
    pub class_names: BTreeMap<i32, String>,
    /// Zeit's ZEIT_EMBEDDING tag of the first file: a layer of foundation-model embeddings
    pub embedding_tag: String,
    /// a variable of a NetCDF / Zarr / HDF file, its bands placed by their coordinates; with more dimensions than
    /// time each combination of the others is a level, sel.band picks the one shown
    pub multidim: bool,
    pub md_variable: String,
    pub md_levels: Vec<String>,
    /// read over the network
    pub remote: bool,
    /// local files whose data are read over the network (a STAC series' VRTs, `remote` false): a pixel of a date
    /// is a few round trips, so a pixel's dates are read several at once
    pub network_sources: bool,
    /// sel.expr compiled over the bands (the reader evaluates it)
    pub expr: Option<Arc<calc::LayerExpr>>,
}

impl CubeInfo {
    pub fn t(&self) -> usize {
        self.layers.len()
    }

    /// The text of a time on the cube's axis: a date at the series' granularity, a wavelength, or the index from 1.
    pub fn format_time(&self, t: f64) -> String {
        if self.spectral {
            format!("{t:.1} nm")
        } else if self.time_is_date {
            dates::format_date(t, self.granularity)
        } else {
            format!("{}", t.round() as i64 + 1)
        }
    }

    /// Decimal year of layer t (e.g. 2003.5); the index when there are no dates.
    pub fn decimal_year(&self, t: usize) -> f64 {
        if self.time_is_date { dates::decimal_year(self.layers[t].time) } else { t as f64 }
    }

    /// Inverse of decimal_year, in the chart's x units (Unix seconds or index).
    pub fn x_from_decimal_year(&self, y: f64) -> f64 {
        if !self.time_is_date {
            return y;
        }
        let yi = y.floor() as i32;
        let days = if dates::is_leap(yi) { 366.0 } else { 365.0 };
        (dates::days_from_civil(yi, 1, 1) as f64 + (y - f64::from(yi)) * days) * 86400.0
    }

    /// Time of layer t in decimal years from the first layer (for trends).
    pub fn years_from_start(&self, t: usize) -> f64 {
        if self.time_is_date { (self.layers[t].time - self.layers[0].time) / (365.2425 * 86400.0) } else { t as f64 }
    }

    /// Days since 1970-01-01 of layer t (dates only).
    pub fn unix_day(&self, t: usize) -> i64 {
        (self.layers[t].time / 86400.0).floor() as i64
    }

    /// The layer whose time (date or wavelength) is nearest to `time`.
    pub fn nearest_layer(&self, time: f64) -> usize {
        let mut best = 0;
        for i in 1..self.t() {
            if (self.layers[i].time - time).abs() < (self.layers[best].time - time).abs() {
                best = i;
            }
        }
        best
    }

    pub fn pixel_to_geo(&self, px: f64, py: f64) -> Option<(f64, f64)> {
        let g = self.geo_transform?;
        Some((g[0] + px * g[1] + py * g[2], g[3] + px * g[4] + py * g[5]))
    }

    pub fn geo_to_pixel(&self, gx: f64, gy: f64) -> Option<(f64, f64)> {
        let g = self.geo_transform?;
        let det = g[1] * g[5] - g[2] * g[4];
        if det == 0.0 {
            return None;
        }
        let (dx, dy) = (gx - g[0], gy - g[3]);
        Some(((dx * g[5] - dy * g[2]) / det, (dy * g[1] - dx * g[4]) / det))
    }

    /// A short text for the shown quantity, e.g. "Band 4" or "ND(nir, red), QA fmask".
    pub fn selection_text(&self) -> String {
        if self.bands_per_date <= 1 {
            return String::new();
        }
        let name = |b: i32| {
            if b >= 1 && (b as usize) <= self.band_names.len() {
                self.band_names[b as usize - 1].clone()
            } else {
                format!("band {b}")
            }
        };
        let mut s = if self.expr.is_some() {
            let l = self.sel.expr.label();
            if l.chars().count() > 40 { format!("{}...", l.chars().take(37).collect::<String>()) } else { l }
        } else if self.sel.nd_band > 0 {
            format!("ND({}, {})", name(self.sel.band), name(self.sel.nd_band))
        } else {
            name(self.sel.band)
        };
        if self.sel.qa_band > 0 && self.sel.qa_rule != QaRule::None {
            s += &format!(", QA {}", name(self.sel.qa_band));
        }
        s
    }
}

/// The same kind of axis (dates, wavelengths or indices): a chart can share it.
pub fn same_axis(a: &CubeInfo, b: &CubeInfo) -> bool {
    a.time_is_date == b.time_is_date && a.spectral == b.spectral
}

/// Both dated or both spectral: the same time or wavelength can be matched.
pub fn matchable_axis(a: &CubeInfo, b: &CubeInfo) -> bool {
    (a.time_is_date && b.time_is_date) || (a.spectral && b.spectral)
}

/// Opens a raster read-only. Remote paths are opened without listing their "folder" (one request less per
/// file; side-car files are not looked for).
pub fn open_raster(path: &str) -> gdal::errors::Result<Dataset> {
    let opts = DatasetOptions {
        open_flags: GdalOpenFlags::GDAL_OF_RASTER | GdalOpenFlags::GDAL_OF_READONLY,
        ..Default::default()
    };
    if !is_remote_path(path) {
        return Dataset::open_ex(path, opts);
    }
    // thread-local: local files keep finding their side-cars (.ovr, .aux.xml, .hdr)
    let key = "GDAL_DISABLE_READDIR_ON_OPEN";
    let old = gdal::config::get_thread_local_config_option(key, "").unwrap_or_default();
    let _ = gdal::config::set_thread_local_config_option(key, "EMPTY_DIR");
    let ds = Dataset::open_ex(path, opts);
    let _ = if old.is_empty() {
        gdal::config::clear_thread_local_config_option(key)
    } else {
        gdal::config::set_thread_local_config_option(key, &old)
    };
    ds
}

pub(crate) fn band_meta(b: &gdal::raster::RasterBand) -> BandMeta {
    BandMeta { no_data: b.no_data_value(), scale: b.scale().unwrap_or(1.0), offset: b.offset().unwrap_or(0.0) }
}

/// The CRS of a dataset: its name, authority and WKT2.
fn read_crs(ds: &Dataset, info: &mut CubeInfo) {
    let Ok(srs) = ds.spatial_ref() else { return };
    info.crs_name = srs.name().unwrap_or_default();
    if let (Some(a), Ok(c)) = (srs.auth_name(), srs.auth_code()) {
        info.crs_authority = format!("{a}:{c}");
    }
    // SAFETY: the handle belongs to `srs`, alive for the call.
    info.crs_wkt = unsafe { wkt2(srs.to_c_hsrs()) };
}

/// # Safety
/// `srs` must be a valid spatial reference handle.
unsafe fn wkt2(srs: gdal_sys::OGRSpatialReferenceH) -> String {
    let opt = CString::new("FORMAT=WKT2_2019").unwrap();
    let opts = [opt.as_ptr(), std::ptr::null()];
    let mut out: *mut std::ffi::c_char = std::ptr::null_mut();
    // SAFETY: GDAL allocates the string, freed with VSIFree.
    unsafe {
        if gdal_sys::OSRExportToWktEx(srs, &mut out, opts.as_ptr()) != 0 || out.is_null() {
            return String::new();
        }
        let s = CStr::from_ptr(out).to_string_lossy().into_owned();
        gdal_sys::VSIFree(out as *mut std::ffi::c_void);
        s
    }
}

/// Classes declared on a band: a colour table, category names, an attribute table.
fn read_class_info(b: &gdal::raster::RasterBand, info: &mut CubeInfo) {
    if let Some(ct) = b.color_table() {
        info.file_categorical = true;
        for i in 0..ct.entry_count().min(65536) {
            if let Some(e) = ct.entry_as_rgb(i) {
                let a = if e.a != 0 { e.a } else { 255 };
                info.class_colors.insert(i as i32, [e.r as u8, e.g as u8, e.b as u8, a as u8]);
            }
        }
    }
    // SAFETY: GDAL's lists and tables belong to the band, read only while it lives.
    unsafe {
        let h = b.c_rasterband();
        let names = gdal_sys::GDALGetRasterCategoryNames(h);
        if !names.is_null() {
            let mut i = 0;
            while !(*names.add(i)).is_null() {
                let s = CStr::from_ptr(*names.add(i)).to_string_lossy().into_owned();
                if !s.is_empty() {
                    info.class_names.insert(i as i32, s);
                }
                i += 1;
            }
            if !info.class_names.is_empty() {
                info.file_categorical = true;
            }
        }
        let rat = gdal_sys::GDALGetDefaultRAT(h);
        if !rat.is_null() {
            use gdal_sys::GDALRATFieldUsage::*;
            let col = |u| gdal_sys::GDALRATGetColOfUsage(rat, u);
            let value = if col(GFU_MinMax) >= 0 { col(GFU_MinMax) } else { col(GFU_Min) };
            let (name, r, g, bl) = (col(GFU_Name), col(GFU_Red), col(GFU_Green), col(GFU_Blue));
            for row in 0..gdal_sys::GDALRATGetRowCount(rat).min(65536) {
                let v = if value >= 0 { gdal_sys::GDALRATGetValueAsInt(rat, row, value) } else { row };
                if name >= 0 {
                    let s = gdal_sys::GDALRATGetValueAsString(rat, row, name);
                    if !s.is_null() && *s != 0 {
                        info.class_names.insert(v, CStr::from_ptr(s).to_string_lossy().into_owned());
                    }
                }
                if r >= 0 && g >= 0 && bl >= 0 {
                    let c = |k| gdal_sys::GDALRATGetValueAsInt(rat, row, k) as u8;
                    info.class_colors.insert(v, [c(r), c(g), c(bl), 255]);
                }
            }
            if name >= 0 || r >= 0 {
                info.file_categorical = true;
            }
        }
    }
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// An embedding dimension's band name: "A00", "A63"...
fn is_dim(s: &str) -> bool {
    s.len() >= 2 && s.starts_with('A') && s[1..].bytes().all(|c| c.is_ascii_digit())
}

/// Interprets the inputs: a folder, a wildcard pattern (*, ?), a list of files or a single file (multiband = 1
/// band per date). `sel` chooses the band(s) used when each file is one date.
pub fn open_cube(inputs: &[String], sel: &BandSelection) -> Result<CubeInfo, String> {
    // the calculator's VRTs compute with a pixel function of ours
    calc::vrt::register();
    let files = inputs::expand_inputs(inputs);
    if files.is_empty() {
        return Err("No raster files found.".into());
    }
    // a STAC series from Planetary Computer: its containers signed before anything is read
    stac::sign_vrt_inputs(&files);
    let open = |f: &str| {
        open_raster(f).map_err(|e| {
            let mut m = format!("{f}: {e}");
            if is_remote_path(f) {
                m += "\n(remote: check the address and the connection; private buckets need credentials, \
                      e.g. AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY or a signed URL)";
            }
            m
        })
    };
    let first = open(&files[0])?;
    let (width, height) = first.raster_size();
    let mut info = CubeInfo {
        id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        width,
        height,
        first_path: files[0].clone(),
        remote: is_remote_path(&files[0]),
        network_sources: is_remote_path(&files[0]) || remote::vrt_reads_network(&files[0]),
        bands_per_date: 1,
        ..Default::default()
    };
    if first.raster_count() == 0 {
        // a container of variables (NetCDF, Zarr, HDF): the interface asks which one; a lone one opens by itself
        let vars = if files.len() == 1 { multidim::list_variables(&files[0]) } else { Vec::new() };
        drop(first);
        if vars.len() == 1 {
            return open_cube(&[vars[0].dataset.clone()], sel);
        }
        if vars.is_empty() {
            return Err(format!("{}: no raster bands.", files[0]));
        }
        let mut e = format!("{} holds {} variables; open one of them:", files[0], vars.len());
        for v in &vars {
            e += &format!("\n  {}", v.dataset);
        }
        return Err(e);
    }
    info.data_type = first.rasterband(1).map(|b| b.band_type().name()).unwrap_or_default();
    info.driver = first.driver().short_name();
    info.geo_transform = first.geo_transform().ok();
    read_crs(&first, &mut info);

    // where each layer's date is looked for
    let mut time_texts: Vec<String> = Vec::new();
    let n = first.raster_count();
    // one file of embeddings: a year with D bands, or zeit.save_raster's every year in one file (bands
    // "<date>_<dimension>", date after date)
    let (mut packed_dates, mut packed_dims) = (0usize, 0usize);
    let (mut packed_prefix, mut packed_names) = (Vec::new(), Vec::new());
    if files.len() == 1 && n >= 16 {
        let tag = first.metadata_item("ZEIT_EMBEDDING", "");
        let descs: Vec<String> = (1..=n)
            .map(|b| first.rasterband(b).ok().and_then(|rb| rb.description().ok()).unwrap_or_default())
            .collect();
        let split: Vec<(String, String)> = descs
            .iter()
            .map(|d| match d.rfind('_') {
                Some(u) => (d[..u].to_string(), d[u + 1..].to_string()),
                None => (String::new(), d.clone()),
            })
            .collect();
        let dims = descs.iter().all(|d| is_dim(d));
        let prefixed = descs.iter().zip(&split).all(|(d, (_, name))| d.contains('_') && is_dim(name));
        if prefixed {
            // dates in blocks of D bands with the same dimension names
            let mut d = 1;
            while d < n && split[d].0 == split[0].0 {
                d += 1;
            }
            let ok = n % d == 0 && (0..n).all(|b| split[b].0 == split[b / d * d].0 && split[b].1 == split[b % d].1);
            if ok && d >= 16 {
                packed_dates = n / d;
                packed_dims = d;
                packed_prefix = (0..packed_dates).map(|g| split[g * d].0.clone()).collect();
                packed_names = split[..d].iter().map(|s| s.1.clone()).collect();
            }
        } else if dims || tag.is_some() {
            // one year
            packed_dates = 1;
            packed_dims = n;
            packed_names = descs
                .iter()
                .enumerate()
                .map(|(b, d)| if d.is_empty() { format!("Band {}", b + 1) } else { d.clone() })
                .collect();
            packed_prefix.push(display_name(&files[0]));
        }
        if packed_dates > 0
            && let Some(t) = tag
        {
            info.embedding_tag = t;
        }
    }

    if packed_dates > 0 {
        info.bands_per_date = packed_dims;
        info.band_names = packed_names;
        info.sel = *sel;
        info.sel.nd_band = info.sel.nd_band.min(packed_dims as i32);
        info.sel.qa_band = 0;
        info.sel.qa_rule = QaRule::None;
        info.sel.band = info.sel.band.clamp(1, packed_dims as i32);
        if sel.expr.is_some() {
            let e = compile_expr(&info, sel)?;
            info.sel.expr = resolved_expr(&info, sel.expr);
            info.sel.band = e.bands.first().copied().unwrap_or(1).clamp(1, packed_dims as i32);
            info.sel.nd_band = 0;
            info.expr = Some(Arc::new(e));
        }
        for (g, prefix) in packed_prefix.iter().enumerate() {
            let base = (g * packed_dims) as i32;
            let band = base + info.sel.band;
            let rb = first.rasterband(band as usize).map_err(|e| e.to_string())?;
            let nd_meta = if info.sel.nd_band > 0 {
                first.rasterband((base + info.sel.nd_band) as usize).map(|b| band_meta(&b)).unwrap_or_default()
            } else {
                BandMeta::default()
            };
            time_texts.push(prefix.clone());
            info.layers.push(Layer {
                path: files[0].clone(),
                band,
                band_base: base,
                label: prefix.clone(),
                meta: band_meta(&rb),
                nd_meta,
                ..Default::default()
            });
        }
        info.description = format!(
            "1 file, {packed_dates} date{} x {packed_dims} bands (embeddings)",
            if packed_dates > 1 { "s" } else { "" }
        );
    } else if files.len() == 1 {
        // one file: each band is one date, or one wavelength of a spectrum
        let nm = spectral::band_wavelengths(&first, &files[0]);
        info.band_nm = nm.clone();
        info.spectral = !nm.is_empty();
        // an index of a spectrum (picked in the Display panel): one map, the normalized difference of two of its
        // bands, dated by the file's name
        if info.spectral && (sel.chosen && sel.nd_band > 0 || sel.expr.is_some()) {
            info.spectral = false;
            info.bands_per_date = n;
            info.band_names = spectral::band_names_with_nm(&first, &nm);
            info.sel = BandSelection {
                band: sel.band.clamp(1, n as i32),
                nd_band: if sel.expr.is_some() { 0 } else { sel.nd_band.clamp(1, n as i32) },
                qa_band: 0,
                qa_rule: QaRule::None,
                chosen: true,
                expr: sel.expr,
            };
            if sel.expr.is_some() {
                let e = compile_expr(&info, sel)?;
                info.sel.expr = resolved_expr(&info, sel.expr);
                info.sel.band = e.bands.first().copied().unwrap_or(1);
                info.expr = Some(Arc::new(e));
            }
            let rb = first.rasterband(info.sel.band as usize).map_err(|e| e.to_string())?;
            let nd_meta = if info.sel.nd_band > 0 {
                band_meta(&first.rasterband(info.sel.nd_band as usize).map_err(|e| e.to_string())?)
            } else {
                BandMeta::default()
            };
            let label = display_name(&files[0]);
            info.layers.push(Layer {
                path: files[0].clone(),
                band: info.sel.band,
                label: label.clone(),
                meta: band_meta(&rb),
                nd_meta,
                ..Default::default()
            });
            info.description = format!("1 file, {} of a spectrum of {n} bands", info.selection_text());
            // the scene's date, if its name has one
            if let Some((t, g)) = dates::parse_date(&label) {
                info.layers[0].time = t;
                info.time_is_date = true;
                info.granularity = g;
                info.layers[0].label = info.format_time(t);
            }
            return Ok(info);
        }
        // a NetCDF / Zarr variable: bands placed by their coordinates (CF time), one level of the others at a time
        if !info.spectral
            && let Some(md) = multidim::read_md_bands(&first, &files[0])
        {
            return open_multidim(info, first, &files[0], md, sel);
        }
        for b in 1..=n {
            let rb = first.rasterband(b).map_err(|e| e.to_string())?;
            if b == 1 && !info.spectral {
                read_class_info(&rb, &mut info);
            }
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
                time: if info.spectral { nm[b - 1] } else { 0.0 },
                meta: band_meta(&rb),
                ..Default::default()
            });
        }
        // compressed with every band in each block: each band read decompresses all of them (~70x slower on a
        // 224-band EnMAP scene)
        let il = first.metadata_item("INTERLEAVE", "IMAGE_STRUCTURE");
        let comp = first.metadata_item("COMPRESSION", "IMAGE_STRUCTURE");
        let slow = il.is_some_and(|v| v.eq_ignore_ascii_case("PIXEL")) && comp.is_some() && n >= 16;
        if info.spectral {
            info.layers.sort_by(|a, b| a.time.total_cmp(&b.time));
            for i in 0..info.t() {
                info.layers[i].label = info.format_time(info.layers[i].time);
            }
            info.description = format!(
                "1 file, {n} bands, {:.0} to {:.0} nm (spectrum){}",
                info.layers[0].time,
                info.layers[info.t() - 1].time,
                if slow {
                    ". Slow to read: compressed pixel-interleaved (gdal_translate -co INTERLEAVE=BAND makes it fast)"
                } else {
                    ""
                }
            );
            return Ok(info);
        }
        info.description = format!("1 file, {n} bands (1 band = 1 date)");
    } else {
        info.bands_per_date = n;
        info.band_nm = spectral::series_band_wavelengths(&first, &files[0]);
        info.band_names = spectral::band_names_with_nm(&first, &info.band_nm);
        info.sel = *sel;
        // bands with wavelengths (e.g. a series of EnMAP scenes): NDVI rather than the first band (418 nm on
        // EnMAP), unless a band was asked for
        if !sel.chosen
            && sel.band == 1
            && sel.nd_band == 0
            && let Some((a, b)) = spectral::index_bands(&info.band_nm, &spectral::INDICES[0])
        {
            info.sel.band = a;
            info.sel.nd_band = b;
        }
        // a quality band recognised by name masks clouds from the start (the Display panel can turn it off)
        if !sel.chosen && sel.qa_band == 0 {
            for (b, name) in info.band_names.iter().enumerate() {
                let l = name.to_lowercase();
                let rule = if l.contains("fmask") {
                    QaRule::Fmask
                } else if l.contains("qa_pixel") {
                    QaRule::LandsatC2
                } else if l == "scl" || l.starts_with("scl ") {
                    QaRule::Scl
                } else {
                    QaRule::None
                };
                if rule != QaRule::None {
                    info.sel.qa_band = b as i32 + 1;
                    info.sel.qa_rule = rule;
                    break;
                }
            }
        }
        if info.sel.qa_band <= 0 {
            info.sel.qa_rule = QaRule::None;
        }
        if info.sel.qa_rule == QaRule::None {
            info.sel.qa_band = 0;
        }
        // an expression: compiled over the bands' names, the first band it reads stands for the shown one
        if sel.expr.is_some() {
            let e = compile_expr(&info, sel)?;
            info.sel.expr = resolved_expr(&info, sel.expr);
            info.sel.band = e.bands.first().copied().unwrap_or(1);
            info.sel.nd_band = 0;
            info.expr = Some(Arc::new(e));
        }
        if let Some(t) = first.metadata_item("ZEIT_EMBEDDING", "") {
            info.embedding_tag = t;
        }
        drop(first);
        // remote files: each open is a few round trips (~1 s), so they are opened 16 at a time
        let mut pre: Vec<Option<Dataset>> = (0..files.len()).map(|_| None).collect();
        if info.remote && files.len() > 1 {
            for chunk in (0..files.len()).collect::<Vec<_>>().chunks(16) {
                let opened: Vec<(usize, Result<Dataset, String>)> = std::thread::scope(|s| {
                    let hs: Vec<_> = chunk
                        .iter()
                        .map(|&i| {
                            let f = files[i].as_str();
                            (i, s.spawn(move || open_raster(f).map_err(|e| e.to_string())))
                        })
                        .collect();
                    hs.into_iter().map(|(i, h)| (i, h.join().unwrap_or_else(|_| Err("panicked".into())))).collect()
                });
                for (i, r) in opened {
                    pre[i] = Some(r.map_err(|e| format!("{}: {e}", files[i]))?);
                }
            }
        }
        let s = info.sel;
        for (fi, f) in files.iter().enumerate() {
            let ds = match pre[fi].take() {
                Some(d) => d,
                None => open(f)?,
            };
            if ds.raster_size() != (width, height) {
                let (w, h) = ds.raster_size();
                return Err(format!(
                    "{}: size {w}x{h} differs from the 1st file ({width}x{height}).",
                    inputs::display_name(f)
                ));
            }
            let need = s.band.max(s.nd_band).max(s.qa_band).max(info.expr.as_ref().map_or(0, |e| e.max_band()));
            if need as usize > ds.raster_count() {
                return Err(format!("{f}: has no band {need}."));
            }
            let rb = ds.rasterband(s.band as usize).map_err(|e| format!("{f}: {e}"))?;
            let nd_meta = if s.nd_band > 0 {
                ds.rasterband(s.nd_band as usize).map(|b| band_meta(&b)).unwrap_or_default()
            } else {
                if info.layers.is_empty() {
                    read_class_info(&rb, &mut info);
                }
                BandMeta::default()
            };
            let label = display_name(f);
            time_texts.push(label.clone());
            info.layers.push(Layer {
                path: f.clone(),
                band: s.band,
                label,
                meta: band_meta(&rb),
                nd_meta,
                ..Default::default()
            });
        }
        info.description = format!(
            "{} files ({} of each)",
            files.len(),
            if info.bands_per_date > 1 { info.selection_text() } else { "band 1".into() }
        );
    }

    // Dates: only used if every layer has one. Files in a folder each, with the date on the folder (Sentinel-2
    // COGs on AWS: .../S2B_23KPQ_20230811_0_L2A/B04.tif), take their folder's.
    let folders: Vec<String> = info
        .layers
        .iter()
        .map(|l| {
            let f = dataset_file(&l.path).split('?').next().unwrap_or("");
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
        match dates::parse_date(text).or_else(|| folder_per_file.then(|| dates::parse_date(&folders[i])).flatten()) {
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
        for i in 0..info.t() {
            info.layers[i].label = info.format_time(info.layers[i].time);
        }
    } else {
        for (i, l) in info.layers.iter_mut().enumerate() {
            l.time = i as f64;
        }
    }
    Ok(info)
}

/// An index of the library given by its name (--expr EVI) as its formula over the cube's bands, under its name:
/// what the Display panel shows and edits.
fn resolved_expr(info: &CubeInfo, expr: calc::ExprId) -> calc::ExprId {
    let text = expr.text();
    let Some(ix) = calc::indices::find(text.trim()) else { return expr };
    match ix.formula_for(&calc::BandNames::of(info), &info.band_nm) {
        Some(f) => calc::ExprId::intern(&ix.name, &f),
        None => expr,
    }
}

/// The selection's expression compiled over the cube's bands (an error says where).
fn compile_expr(info: &CubeInfo, sel: &BandSelection) -> Result<calc::LayerExpr, String> {
    let names = calc::BandNames::of(info);
    let mut text = sel.expr.text();
    // an index of the library by its name (--expr EVI): its formula over these bands
    if let Some(ix) = calc::indices::find(text.trim()) {
        if !ix.usable(&names) {
            return Err(format!("{} needs bands this series lacks: {}", ix.title(), ix.missing(&names).join(", ")));
        }
        text =
            ix.formula_for(&names, &info.band_nm).ok_or_else(|| format!("{} needs a value no band gives", ix.name))?;
    }
    calc::LayerExpr::compile(&text, &names).map_err(|e| {
        let (line, col) = e.line_col(&text);
        format!("the expression {text:?}: {} (line {line}, column {col})", e.message)
    })
}

/// A NetCDF / Zarr variable: one level's bands, by their time coordinate.
fn open_multidim(
    mut info: CubeInfo,
    ds: Dataset,
    path: &str,
    md: multidim::MdBands,
    sel: &BandSelection,
) -> Result<CubeInfo, String> {
    info.multidim = true;
    let (var, file) = (display_name(path), display_name(dataset_file(path)));
    info.md_variable = if var.len() > file.len() + 1 { var[file.len() + 1..].to_string() } else { String::new() };
    if info.md_variable.is_empty()
        && let Some(v) = ds.rasterband(1).ok().and_then(|b| b.metadata_item("NETCDF_VARNAME", ""))
    {
        info.md_variable = v; // a file of one variable, opened by its name
    }
    if md.levels.len() > 1 {
        info.md_levels = md.levels.clone();
    }
    info.sel = BandSelection {
        band: sel.band.clamp(1, md.levels.len().max(1) as i32),
        chosen: sel.chosen,
        ..Default::default()
    };
    for b in 1..=ds.raster_count() {
        if md.level[b - 1] != info.sel.band as usize - 1 {
            continue;
        }
        let Ok(rb) = ds.rasterband(b) else { continue };
        if info.layers.is_empty() {
            read_class_info(&rb, &mut info);
        }
        info.layers.push(Layer {
            path: path.to_string(),
            band: b as i32,
            time: md.time[b - 1],
            meta: band_meta(&rb),
            ..Default::default()
        });
    }
    if info.layers.is_empty() {
        return Err(format!("{path}: no bands for the chosen level."));
    }
    info.layers.sort_by(|a, b| a.time.total_cmp(&b.time));
    if md.time_is_date {
        info.time_is_date = true;
        info.granularity = dates::granularity_of(info.layers.iter().map(|l| l.time));
        for i in 0..info.t() {
            info.layers[i].label = info.format_time(info.layers[i].time);
        }
    } else {
        // the axis' coordinate, as an index
        for (i, l) in info.layers.iter_mut().enumerate() {
            l.label = format!("{} {}", md.time_dim, multidim::format_g6(l.time));
            l.time = i as f64;
        }
    }
    // lon/lat grids without a grid mapping (most climate files): WGS 84, so basemaps, vectors and other layers
    // line up
    if let Some(g) = info.geo_transform
        && info.crs_wkt.is_empty()
    {
        let (x1, y1) = (g[0] + g[1] * info.width as f64, g[3] + g[5] * info.height as f64);
        if g[0].min(x1) >= -180.5
            && g[0].max(x1) <= 360.5
            && g[3].min(y1) >= -90.5
            && g[3].max(y1) <= 90.5
            && let Ok(wgs) = gdal::spatial_ref::SpatialRef::from_epsg(4326)
        {
            // SAFETY: the handle belongs to `wgs`, alive for the call.
            info.crs_wkt = unsafe { wkt2(wgs.to_c_hsrs()) };
            info.crs_name = "WGS 84 (assumed: lon/lat grid without a grid mapping)".into();
            info.crs_authority = "EPSG:4326".into();
        }
    }
    let mut d = format!(
        "{} variable{}, {}{}",
        info.driver,
        if info.md_variable.is_empty() { String::new() } else { format!(" {}", info.md_variable) },
        info.t(),
        if md.time_is_date { " dates".to_string() } else { format!(" steps of {}", md.time_dim) }
    );
    if !info.md_levels.is_empty() {
        d += &format!(", {}", info.md_levels[info.sel.band as usize - 1]);
    }
    let cal = md.calendar.to_lowercase();
    if !md.calendar.is_empty() && cal != "standard" && cal != "gregorian" && cal != "proleptic_gregorian" {
        d += &format!(" ({} calendar)", md.calendar);
    }
    info.description = d;
    Ok(info)
}

/// What files would open as, from their names alone (dates as open_cube reads them): "41 rasters, 1985 to 2025,
/// yearly"; empty without files.
pub fn describe_series_files(names: &[String]) -> String {
    if names.is_empty() {
        return String::new();
    }
    let stem =
        |n: &str| std::path::Path::new(n).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    if let Some(n) = names.iter().find(|n| remote::is_enmap_spectral_image(std::path::Path::new(n))) {
        let d = dates::parse_date(&stem(n)).map(|(t, _)| format!(", {}", dates::format_date(t, TimeGranularity::Day)));
        return format!("EnMAP scene{} (opens its spectrum)", d.unwrap_or_default());
    }
    let parsed: Vec<(f64, TimeGranularity)> = names.iter().filter_map(|n| dates::parse_date(&stem(n))).collect();
    let n = names.len();
    if parsed.len() < n {
        return if parsed.is_empty() {
            format!("{n} rasters, no dates in the names (ordered by name)")
        } else {
            format!("{n} rasters, dates in {} of the names (ordered by name)", parsed.len())
        };
    }
    let gran = parsed.iter().map(|p| p.1).fold(TimeGranularity::Year, Ord::max);
    let mut times: Vec<f64> = parsed.iter().map(|p| p.0).collect();
    times.sort_by(f64::total_cmp);
    if n == 1 {
        return format!("1 raster, {}", dates::format_date(times[0], gran));
    }
    let mut gaps: Vec<f64> = times.windows(2).map(|w| (w[1] - w[0]) / 86400.0).collect();
    let m = gaps.len() / 2;
    let gap = *gaps.select_nth_unstable_by(m, f64::total_cmp).1;
    let every = if (360.0..=370.0).contains(&gap) {
        "yearly".to_string()
    } else if (28.0..=31.0).contains(&gap) {
        "monthly".into()
    } else if (0.5..=1.5).contains(&gap) {
        "daily".into()
    } else if gap > 0.0 {
        format!("every ~{} days", gap.round() as i64)
    } else {
        "several files share a date".into()
    };
    format!(
        "{n} rasters, {} to {}, {every}",
        dates::format_date(times[0], gran),
        dates::format_date(times[n - 1], gran)
    )
}

/// The version of the GDAL in use.
pub fn gdal_version() -> String {
    gdal::version::version_info("RELEASE_NAME")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_rules() {
        assert!(QaRule::Fmask.usable(1) && !QaRule::Fmask.usable(4));
        // Landsat C2: bit 3 (cloud) masks, bit 6 (clear) does not
        assert!(!QaRule::LandsatC2.usable(1 << 3) && QaRule::LandsatC2.usable(1 << 6));
        assert!(QaRule::NonZero.usable(7) && !QaRule::NonZero.usable(0));
        // SCL: vegetation, soil and water kept; cloud shadow (3), clouds (8, 9), cirrus (10), snow (11) masked
        assert!([4, 5, 6].iter().all(|&c| QaRule::Scl.usable(c)));
        assert!([0, 1, 2, 3, 7, 8, 9, 10, 11].iter().all(|&c| !QaRule::Scl.usable(c)));
    }

    #[test]
    fn describe_names() {
        let names: Vec<String> = (1985..=2025).map(|y| format!("landsat_medoid_{y}.tif")).collect();
        assert_eq!(describe_series_files(&names), "41 rasters, 1985 to 2025, yearly");
        assert_eq!(
            describe_series_files(&["a.tif".into(), "b.tif".into()]),
            "2 rasters, no dates in the names (ordered by name)"
        );
    }
}
