//! The inputs of Zeit's Level 2 correction of a Sentinel-2 L1C scene (`zeit.level2`, FORCE's `force-l2ps`), read and
//! prepared in Rust as `zeit/_level2.py` does, ahead of zeit-core's `level2_scene` (the next step: this module does not
//! run it). Each function is the port of its namesake in `_level2.py` and gives the same values, bit for bit
//! (tools/level2_inputs_fixtures.py writes Zeit's arrays, tests/level2_inputs.rs compares them):
//!
//! * [`Safe`] and [`Safe::describe`]: the files of a `.SAFE` directory and the metadata the core reads (zeit-core's
//!   `s2l1c::parse_metadata`, `l2scene::geo_grid`, as `_core.l2scene.describe` calls them);
//! * [`read_bands`]: the 13 bands as `u16` on the grid of the finest band, each read in its window and resampled by
//!   GDAL's nearest neighbour (rasterio's `read(window=, out_shape=, resampling=nearest)` is the same `RasterIO`);
//! * [`geo_tables`]: the longitude and latitude of the corners of the coarse grid, by OGR from the CRS of the bands;
//! * [`prepare`]: the initial QAI and the TOA reflectance (`bounds_level1`, `convert_level1`);
//! * [`dem_on_grid`]: the DEM on the grid of a band by GDAL's own warp (`GDALWarp`, bilinear, `-et 0`: every pixel
//!   transformed exactly, which is what Zeit's `to_grid(..., tolerance=0)`, the port of GDAL's warp kernel, computes);
//! * [`view_angles_20m`], [`fmask_flags`]: Zeit's Fmask 4.7 (zeit-core's `fmask::run`, as `_core.fmask_run` calls it)
//!   on the bands decimated to 20 m, with the angles of the `MTD_TL.xml`, the DEM and, optionally, the GSWO.
//!
//! Every thread count is capped by the `n_jobs` given (GDAL's `GDAL_NUM_THREADS` too: a JPEG 2000 decoder would take
//! every core otherwise).

use std::borrow::Cow;
use std::ffi::{CString, c_char};
use std::path::{Path, PathBuf};
use std::ptr::null_mut;

use gdal::raster::{GdalType, ResampleAlg};
use gdal::spatial_ref::{AxisMappingStrategy, CoordTransform, SpatialRef};
use gdal::{Dataset, DriverManager};
use quick_xml::Reader;
use quick_xml::events::Event;
use zeit_core::fmask::detect::FmaskParameters;
use zeit_core::fmask::run::{self, DnBands, Radiometry, RunConfig, RunInput};
use zeit_core::l2scene::{self, GeoGrid};
use zeit_core::s2l1c::{self, L1c, NB};

use crate::s2angles;
use crate::warp::Grid;

/// The band files of the product in the order of FORCE (`orig_band` of `meta-ll.c`).
pub const BAND_NAMES: [&str; NB] = ["01", "02", "03", "04", "05", "06", "07", "08", "8A", "09", "10", "11", "12"];

/// The no-data value of a DEM on the grid of the scene (what FORCE's `GDALWarpOperation` hands to the topography).
pub const DEM_NODATA: i32 = -32767;

/// Indices in [`BAND_NAMES`] of the bands the Fmask reads, in the argument order of `dn_to_toa20`: B02, B03, B04 (10 m),
/// B8A, B11, B12 (20 m), B10 (60 m), B07 (20 m), B08 (10 m).
pub const FMASK_BANDS: [usize; 9] = [1, 2, 3, 8, 11, 12, 10, 6, 7];

/// The step by which each of [`FMASK_BANDS`] is decimated from the 10 m grid to the 20 m one.
pub const FMASK_STEP: [usize; 9] = [1, 1, 1, 2, 2, 2, 6, 2, 1];

// --- the product -------------------------------------------------------------------------------------------------

/// The files of a Level 1C `.SAFE` directory that the correction reads (`Safe`).
#[derive(Clone, Debug)]
pub struct Safe {
    pub path: PathBuf,
    pub product: PathBuf,
    pub granule: PathBuf,
    pub tile: PathBuf,
    /// The 13 band files in the order of [`BAND_NAMES`] (JPEG 2000, or GeoTIFFs under the same names).
    pub bands: Vec<PathBuf>,
}

impl Safe {
    /// `MTD_MSIL1C.xml`, the single `GRANULE/<name>/`, its `MTD_TL.xml` and the band files `*_B{nn}.jp2` or `.tif`.
    pub fn open(path: &Path) -> Result<Safe, String> {
        let product = path.join("MTD_MSIL1C.xml");
        let mut granules: Vec<PathBuf> = match std::fs::read_dir(path.join("GRANULE")) {
            Ok(d) => d.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect(),
            Err(_) => vec![],
        };
        granules.sort();
        if !product.is_file() || granules.len() != 1 {
            return Err(format!(
                "{}: not a Sentinel-2 L1C .SAFE directory (MTD_MSIL1C.xml and one GRANULE/<name>/ expected)",
                path.display()
            ));
        }
        let granule = granules.remove(0);
        let tile = granule.join("MTD_TL.xml");
        if !tile.is_file() {
            return Err(format!("{}: no MTD_TL.xml", granule.display()));
        }
        let img = granule.join("IMG_DATA");
        let files: Vec<PathBuf> = match std::fs::read_dir(&img) {
            Ok(d) => d.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect(),
            Err(_) => vec![],
        };
        let mut bands = Vec::with_capacity(NB);
        for name in BAND_NAMES {
            let (jp2, tif) = (format!("_b{name}.jp2").to_lowercase(), format!("_b{name}.tif").to_lowercase());
            let mut found: Vec<&PathBuf> = files
                .iter()
                .filter(|f| {
                    let n = f.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
                    n.ends_with(&jp2) || n.ends_with(&tif)
                })
                .collect();
            found.sort();
            match found.first() {
                Some(f) => bands.push((*f).clone()),
                None => return Err(format!("{}: no band file *_B{name}.jp2", img.display())),
            }
        }
        Ok(Safe { path: path.to_path_buf(), product, granule, tile, bands })
    }

    /// The name of the granule directory (the core takes the tile's identifier from it).
    pub fn granule_name(&self) -> String {
        self.granule.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// The metadata the core reads (`_core.l2scene.describe`): grid, date, corners and read windows.
    pub fn describe(&self) -> Result<Desc, String> {
        let product = std::fs::read(&self.product).map_err(|e| format!("{}: {e}", self.product.display()))?;
        let tile = std::fs::read(&self.tile).map_err(|e| format!("{}: {e}", self.tile.display()))?;
        let l1 = s2l1c::parse_metadata(&product, &tile, &self.granule_name(), false).map_err(|e| e.to_string())?;
        let geo_grid = l2scene::geo_grid(&l1).map_err(|e| e.to_string())?;
        let windows = [10.0, 20.0, 60.0].iter().map(|&r| (r, l1.read_window(r))).collect();
        Ok(Desc { nrows: l1.tile.nrows as usize, ncols: l1.tile.ncols as usize, l1, geo_grid, windows })
    }
}

/// What `describe` gives: the parsed metadata (`l1`: the grid of the scene, the date, `radd`, the angle grids...), the
/// corners the correction needs the geography of and the window to read from a band of each resolution.
#[derive(Clone, Debug)]
pub struct Desc {
    pub l1: L1c,
    pub nrows: usize,
    pub ncols: usize,
    pub geo_grid: GeoGrid,
    /// `(resolution, (xoff, yoff, nx, ny))` for 10, 20 and 60 m.
    pub windows: Vec<(f64, (i32, i32, i32, i32))>,
}

impl Desc {
    /// The window (`xoff, yoff, nx, ny`) of the resolution of the list closest to `res` (the first on a tie).
    pub fn window_for(&self, res: f64) -> (i32, i32, i32, i32) {
        let mut best = &self.windows[0];
        for w in &self.windows {
            if (w.0 - res).abs() < (best.0 - res).abs() {
                best = w;
            }
        }
        best.1
    }
}

// --- the bands ---------------------------------------------------------------------------------------------------

fn gdal_err(path: &Path, e: impl std::fmt::Display) -> String {
    format!("{}: {e}", path.display())
}

fn threads(n_jobs: usize) -> usize {
    n_jobs.clamp(1, 8)
}

/// The 13 bands as `u16`, band by band (`13 * nrows * ncols` values) on the grid of the finest band: each read in its
/// window and resampled to the grid by GDAL's nearest neighbour (`read_bands`). `n_jobs` caps the threads of the JPEG 2000
/// decoder (`GDAL_NUM_THREADS`), which would otherwise take every core.
pub fn read_bands(safe: &Safe, desc: &Desc, n_jobs: usize) -> Result<Vec<u16>, String> {
    let (nr, nc) = (desc.nrows, desc.ncols);
    let n = nr * nc;
    let mut out = vec![0u16; NB * n];
    gdal::config::set_thread_local_config_option("GDAL_NUM_THREADS", &threads(n_jobs).to_string())
        .map_err(|e| e.to_string())?;
    for (b, f) in safe.bands.iter().enumerate() {
        let ds = Dataset::open(f).map_err(|e| gdal_err(f, e))?;
        let res = ds.geo_transform().map_err(|e| gdal_err(f, e))?[1].abs();
        let (xoff, yoff, nx, ny) = desc.window_for(res);
        let band = ds.rasterband(1).map_err(|e| gdal_err(f, e))?;
        band.read_into_slice::<u16>(
            (xoff as isize, yoff as isize),
            (nx as usize, ny as usize),
            (nc, nr),
            &mut out[b * n..(b + 1) * n],
            Some(ResampleAlg::NearestNeighbour),
        )
        .map_err(|e| gdal_err(f, e))?;
    }
    Ok(out)
}

fn srs_of(ds: &Dataset, path: &Path) -> Result<SpatialRef, String> {
    let mut srs = ds.spatial_ref().map_err(|e| gdal_err(path, e))?;
    srs.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    Ok(srs)
}

/// Longitude and latitude (degrees, `f64`, row by row) of the corners `desc.geo_grid` lists, from the CRS of the first
/// band file (`geo_tables`; traditional GIS axis order, as rasterio's).
pub fn geo_tables(safe: &Safe, desc: &Desc) -> Result<(Vec<f64>, Vec<f64>), String> {
    let g = desc.geo_grid;
    let f = &safe.bands[0];
    let ds = Dataset::open(f).map_err(|e| gdal_err(f, e))?;
    let from = srs_of(&ds, f)?;
    let mut to = SpatialRef::from_epsg(4326).map_err(|e| e.to_string())?;
    to.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    let tr = CoordTransform::new(&from, &to).map_err(|e| e.to_string())?;
    let mut x: Vec<f64> = (0..g.nx * g.ny).map(|k| g.ulx + (k % g.nx) as f64 * g.res).collect();
    let mut y: Vec<f64> = (0..g.nx * g.ny).map(|k| g.uly + (k / g.nx) as f64 * -g.res).collect();
    // SAFETY: x and y are n long each and live through the call; `tr` owns the transformation; no z.
    let ok =
        unsafe { gdal_sys::OCTTransform(tr.to_c_hct(), x.len() as i32, x.as_mut_ptr(), y.as_mut_ptr(), null_mut()) };
    if ok == 0 {
        return Err("the corners of the coarse grid could not be put in longitude and latitude".into());
    }
    Ok((x, y))
}

/// The initial QAI (`nrows * ncols`) and the TOA reflectance (13 bands of `int16`, x 10000) the correction starts from
/// (`bounds_level1` and `convert_level1`), from the `dn` of [`read_bands`].
pub fn prepare(desc: &Desc, dn: &[u16], n_jobs: usize) -> Result<(Vec<i16>, Vec<Vec<i16>>), String> {
    let n = desc.nrows * desc.ncols;
    if dn.len() != NB * n {
        return Err(format!("dn has {} values, not 13 x {n}", dn.len()));
    }
    let bands: Vec<&[u16]> = dn.chunks(n.max(1)).collect();
    desc.l1.bounds_and_convert(&bands, false, threads(n_jobs) as i32).map_err(|e| e.to_string())
}

// --- the DEM and other rasters on the grid of a band --------------------------------------------------------------

/// The grid of the band file whose resolution is closest to `resolution` (the finest if None); with `desc`, cropped to the
/// read window of that resolution (the image of the correction), else the whole file (`_band_grid`).
pub fn band_grid(safe: &Safe, resolution: Option<f64>, desc: Option<&Desc>) -> Result<Grid, String> {
    let mut best: Option<(f64, f64, &PathBuf)> = None;
    for f in &safe.bands {
        let ds = Dataset::open(f).map_err(|e| gdal_err(f, e))?;
        let res = ds.geo_transform().map_err(|e| gdal_err(f, e))?[1].abs();
        let key = resolution.map_or(res, |r| (res - r).abs());
        if best.is_none_or(|b| key < b.0) {
            best = Some((key, res, f));
        }
    }
    let (_, res, f) = best.ok_or("no band files")?;
    let ds = Dataset::open(f).map_err(|e| gdal_err(f, e))?;
    let mut gt = ds.geo_transform().map_err(|e| gdal_err(f, e))?;
    let wkt = ds.spatial_ref().and_then(|s| s.to_wkt()).map_err(|e| gdal_err(f, e))?;
    let (mut w, mut h) = ds.raster_size();
    if let Some(d) = desc {
        // the transform of the file times a translation by the window's origin (Affine's product)
        let (xoff, yoff, nx, ny) = d.window_for(res);
        gt[0] += gt[1] * xoff as f64 + gt[2] * yoff as f64;
        gt[3] += gt[4] * xoff as f64 + gt[5] * yoff as f64;
        (w, h) = (nx as usize, ny as usize);
    }
    Ok(Grid { wkt, gt, w, h })
}

/// A raster of any CRS and cell on `grid`, by GDAL's own warp (`GDALWarp` onto a dataset of that grid, `-et 0`, so that every
/// pixel is transformed exactly: the values of Zeit's `to_grid(..., tolerance=0)`), `T` the type of the result, `nodata`
/// the no-data value of the result (and of the source too when the source has none: `default_src_nodata`).
fn warp_to_grid<T: Copy + GdalType + Default>(
    src: &Path,
    grid: &Grid,
    method: &str,
    default_src_nodata: f64,
    nodata: f64,
    n_jobs: usize,
) -> Result<Vec<T>, String> {
    let ds = Dataset::open(src).map_err(|e| gdal_err(src, e))?;
    let src_nodata = ds.rasterband(1).map_err(|e| gdal_err(src, e))?.no_data_value();
    let mut dst = DriverManager::get_driver_by_name("MEM")
        .and_then(|d| d.create_with_band_type::<T, _>("", grid.w, grid.h, 1))
        .map_err(|e| e.to_string())?;
    dst.set_geo_transform(&grid.gt).map_err(|e| e.to_string())?;
    dst.set_projection(&grid.wkt).map_err(|e| e.to_string())?;
    {
        let mut band = dst.rasterband(1).map_err(|e| e.to_string())?;
        band.set_no_data_value(Some(nodata)).map_err(|e| e.to_string())?;
        band.fill(nodata, None).map_err(|e| e.to_string())?;
    }
    let mut args: Vec<String> = ["-r", method, "-et", "0", "-dstnodata"].iter().map(|s| s.to_string()).collect();
    args.push(format!("{nodata:?}"));
    if src_nodata.is_none() {
        args.push("-srcnodata".into());
        args.push(format!("{default_src_nodata:?}"));
    }
    args.push("-wo".into());
    args.push(format!("NUM_THREADS={}", threads(n_jobs)));
    let c = |s: &str| CString::new(s).map_err(|_| format!("{s}: a NUL in it"));
    // SAFETY: GDAL's C API: the argument list and options made and freed here, the source and destination alive throughout
    // (GDALWarp onto an existing dataset returns that same handle, which is not closed here).
    unsafe {
        let mut argv: *mut *mut c_char = null_mut();
        for a in &args {
            argv = gdal_sys::CSLAddString(argv, c(a)?.as_ptr());
        }
        gdal_sys::CPLErrorReset();
        let o = gdal_sys::GDALWarpAppOptionsNew(argv, null_mut());
        gdal_sys::CSLDestroy(argv);
        if o.is_null() {
            return Err(format!("gdalwarp's options: {}", crate::polygonize::last_error()));
        }
        let mut src_h = [ds.c_dataset()];
        let mut usage = 0;
        let out = gdal_sys::GDALWarp(std::ptr::null(), dst.c_dataset(), 1, src_h.as_mut_ptr(), o, &mut usage);
        gdal_sys::GDALWarpAppOptionsFree(o);
        if out.is_null() {
            let e = crate::polygonize::last_error();
            return Err(if e.is_empty() { "gdalwarp failed".into() } else { e });
        }
    }
    let mut v = vec![T::default(); grid.w * grid.h];
    dst.rasterband(1)
        .and_then(|b| b.read_into_slice::<T>((0, 0), (grid.w, grid.h), (grid.w, grid.h), &mut v, None))
        .map_err(|e| e.to_string())?;
    Ok(v)
}

/// The DEM on the grid of the scene, `f32` `nrows * ncols` of the grid in metres, [`DEM_NODATA`] where it has no data
/// (`dem_on_grid`: bilinear, the source's no-data value, or -32767 if it has none, as the destination's too).
///
/// `resolution` picks the grid of the band of that resolution (20: B11, for the Fmask) instead of the finest one, and `desc`
/// crops it to the read window of the scene (both None: the whole 10 m file).
pub fn dem_on_grid(
    dem: &Path,
    safe: &Safe,
    resolution: Option<f64>,
    desc: Option<&Desc>,
    n_jobs: usize,
) -> Result<Vec<f32>, String> {
    dem_on_grid_with(dem, safe, resolution, desc, "bilinear", n_jobs)
}

/// [`dem_on_grid`] with the resampling of `method` (GDAL's name: `bilinear`, `near`, `cubic`...; `dem_resampling` of `level2`).
pub fn dem_on_grid_with(
    dem: &Path,
    safe: &Safe,
    resolution: Option<f64>,
    desc: Option<&Desc>,
    method: &str,
    n_jobs: usize,
) -> Result<Vec<f32>, String> {
    let grid = band_grid(safe, resolution, desc)?;
    warp_to_grid::<f32>(dem, &grid, method, f64::from(DEM_NODATA), f64::from(DEM_NODATA), n_jobs)
}

/// The Global Surface Water Occurrence raster (percent) on the 20 m grid by nearest neighbour, `u8`, 0 where it has no
/// data (`_gswo_on_grid`).
pub fn gswo_on_grid(gswo: &Path, safe: &Safe, desc: &Desc, n_jobs: usize) -> Result<Vec<u8>, String> {
    let grid = band_grid(safe, Some(20.0), Some(desc))?;
    warp_to_grid::<u8>(gswo, &grid, "near", 255.0, 0.0, n_jobs)
}

// --- the angles of the metadata ----------------------------------------------------------------------------------

/// `Mean_Sun_Angle` of an `MTD_TL.xml`: the zenith and azimuth (degrees) of the whole scene (`_mean_sun_angle`).
pub fn mean_sun_angle(xml: &str) -> Result<(f64, f64), String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let (mut inside, mut depth) = (false, 0usize);
    let mut current = String::new();
    let (mut zenith, mut azimuth) = (None, None);
    loop {
        match reader.read_event().map_err(|e| format!("MTD_TL.xml: not XML: {e}"))? {
            Event::Start(e) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                if !inside && name == "Mean_Sun_Angle" {
                    inside = true;
                } else if inside {
                    depth += 1;
                    if depth == 1 {
                        current = name;
                    }
                }
            }
            Event::Text(t) if inside && depth == 1 => {
                let text = t.decode().map_err(|e| format!("MTD_TL.xml: not XML: {e}"))?;
                let v = text
                    .trim()
                    .parse::<f64>()
                    .map_err(|_| format!("MTD_TL.xml: {current} is not a number: {text:?}"))?;
                match current.as_str() {
                    "ZENITH_ANGLE" => zenith = Some(v),
                    "AZIMUTH_ANGLE" => azimuth = Some(v),
                    _ => {}
                }
            }
            Event::End(_) if inside => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            Event::Eof => return Err("MTD_TL.xml: no Mean_Sun_Angle".into()),
            _ => {}
        }
    }
    match (zenith, azimuth) {
        (Some(z), Some(a)) => Ok((z, a)),
        _ => Err("MTD_TL.xml: Mean_Sun_Angle without ZENITH_ANGLE and AZIMUTH_ANGLE".into()),
    }
}

/// numpy's pairwise sum of `f32` values (`pairwise_sum` of its `loops_utils.h`), as `add.reduce` does after taking the
/// first value as the start: the sum numpy's `mean` of a `float32` array gives.
fn numpy_sum_f32(a: &[f32]) -> f32 {
    fn pairwise(a: &[f32]) -> f32 {
        let n = a.len();
        if n < 8 {
            let mut res = 0.0f32;
            for v in a {
                res += v;
            }
            res
        } else if n <= 128 {
            let mut r = [a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7]];
            let mut i = 8;
            while i < n - (n % 8) {
                for j in 0..8 {
                    r[j] += a[i + j];
                }
                i += 8;
            }
            let mut res = ((r[0] + r[1]) + (r[2] + r[3])) + ((r[4] + r[5]) + (r[6] + r[7]));
            while i < n {
                res += a[i];
                i += 1;
            }
            res
        } else {
            let mut n2 = n / 2;
            n2 -= n2 % 8;
            pairwise(&a[..n2]) + pairwise(&a[n2..])
        }
    }
    match a.split_first() {
        None => 0.0,
        Some((first, rest)) => *first + pairwise(rest),
    }
}

/// The view zenith and azimuth of B8A on the 20 m grid of the window in hundredths of a degree (`f32`, what `run_fmask`
/// reads), from the 5 km grids of `MTD_TL.xml`, and the fraction of pixels no detector covered, which take the mean of the
/// others (`_view_angles_20m`).
pub fn view_angles_20m(safe: &Safe, desc: &Desc) -> Result<(Vec<f32>, Vec<f32>, f64), String> {
    let (rows, cols) = (desc.nrows / 2, desc.ncols / 2);
    let g = band_grid(safe, Some(20.0), Some(desc))?;
    if (g.h, g.w) != (rows, cols) {
        return Err(format!("the 20 m grid of the window is {} x {}, not {rows} x {cols}", g.h, g.w));
    }
    let xml = std::fs::read_to_string(&safe.tile).map_err(|e| gdal_err(&safe.tile, e))?;
    let tile = s2angles::read(&xml, &safe.tile.to_string_lossy(), s2angles::band_id("B8A").ok_or("B8A")?)?;
    let like = s2angles::Like::new(&g.wkt, g.gt)?;
    let [_, _, vza, vaa] = s2angles::angles(&[&tile], &like, [0, 0, cols, rows])?;
    let mut out = vec![];
    let mut missing = 0.0f64;
    for mut a in [vza, vaa] {
        let good: Vec<f32> = a.iter().copied().filter(|v| v.is_finite()).collect();
        if good.is_empty() {
            return Err("MTD_TL.xml: no view angle covers the scene".into());
        }
        missing = missing.max((a.len() - good.len()) as f64 / a.len() as f64);
        if good.len() < a.len() {
            let mean = numpy_sum_f32(&good) / good.len() as f32;
            for v in a.iter_mut().filter(|v| !v.is_finite()) {
                *v = mean;
            }
        }
        for v in a.iter_mut() {
            *v *= 100.0;
        }
        out.push(a);
    }
    let vaa = out.pop().expect("two");
    let vza = out.pop().expect("one");
    Ok((vza, vaa, missing))
}

// --- the Fmask -----------------------------------------------------------------------------------------------------

/// Where the DEM of [`fmask_flags`] comes from.
#[derive(Clone, Copy, Debug)]
pub enum FmaskDem<'a> {
    /// A raster of any grid, put on the 20 m grid by [`dem_on_grid`].
    File(&'a Path),
    /// An array on the 20 m grid (`nrows / 2 * ncols / 2`) or on the 10 m one (taken by `[::2, ::2]`), in metres, with
    /// [`DEM_NODATA`] where it has no data.
    Array(&'a [f32]),
}

/// What [`fmask_flags`] did besides the flags.
#[derive(Clone, Debug)]
pub struct FmaskInfo {
    pub sum_clr: usize,
    pub few_clear: bool,
    pub match_skipped: bool,
    pub clr_pct: f64,
    pub observed: f64,
    pub cloud: f64,
    pub shadow: f64,
    pub snow: f64,
    pub water: f64,
    /// `baseline04` or `legacy`, the radiometry of `nd2toarbt_msi2.m` or the older one.
    pub route: &'static str,
    pub sun_zenith: f64,
    pub sun_azimuth: f64,
    pub view_missing: f64,
}

/// `dn[band]` decimated by `step` from a grid of `nr x nc`.
fn decimate(dn: &[u16], nr: usize, nc: usize, step: usize) -> Cow<'_, [u16]> {
    if step == 1 {
        return Cow::Borrowed(dn);
    }
    let mut out = Vec::with_capacity((nr / step) * (nc / step));
    for r in (0..nr).step_by(step) {
        out.extend(dn[r * nc..(r + 1) * nc].iter().step_by(step));
    }
    Cow::Owned(out)
}

/// The cloud, shadow, snow and water bits of the QAI (`nrows * ncols`, 10 m) from Zeit's own Fmask 4.7, and what it did
/// (`fmask_flags`). `dn` is the `13 * nrows * ncols` `u16` of [`read_bands`]. The Fmask runs at 20 m, on the grid of the
/// window of the scene: the 10 m bands (B02, B03, B04, B08) go in whole, the 20 m ones (B8A, B11, B12, B07) by `[::2, ::2]`
/// and B10 (60 m) by `[::6, ::6]` (so `nrows` and `ncols` must be multiples of 6, as the 10980 of a tile are). The radiometry
/// is that of `nd2toarbt_msi2.m` from processing baseline 04.00 on and the old one before. The sun is the `Mean_Sun_Angle`
/// of the `MTD_TL.xml` and the view angles those of B8A per pixel. The cloud buffer of the output is 300 m and cirrus is
/// never flagged (as in FORCE).
pub fn fmask_flags(
    safe: &Safe,
    desc: &Desc,
    dn: &[u16],
    dem: Option<FmaskDem>,
    gswo: Option<&Path>,
    compat: bool,
    n_jobs: usize,
) -> Result<(Vec<i16>, FmaskInfo), String> {
    let (nr, nc) = (desc.nrows, desc.ncols);
    let n = nr * nc;
    if dn.len() != NB * n {
        return Err(format!("dn has {} values, expected 13 x {n}", dn.len()));
    }
    if nr % 6 != 0 || nc % 6 != 0 {
        return Err(format!("the Fmask needs the scene's rows and columns to be multiples of 6, got {nr} x {nc}"));
    }
    let (rows, cols) = (nr / 2, nc / 2);
    let n20 = rows * cols;
    let bands: Vec<Cow<[u16]>> =
        FMASK_BANDS.iter().zip(FMASK_STEP).map(|(&b, k)| decimate(&dn[b * n..(b + 1) * n], nr, nc, k)).collect();

    let p = &desc.l1.product;
    let new = match p.baseline.as_deref().unwrap_or("None").trim().parse::<f64>() {
        Ok(v) => v >= 4.0,
        Err(_) => p.radd.iter().any(|&v| v != 0.0),
    };
    let radio = if new {
        let mut offsets = [0f64; 9];
        for (o, &b) in offsets.iter_mut().zip(&FMASK_BANDS) {
            *o = f64::from(p.radd[b]);
        }
        Radiometry::Baseline04 { offsets, quantification: f64::from(p.dn_scale) }
    } else {
        Radiometry::Legacy
    };
    let toa = run::dn_to_toa20(
        &DnBands {
            b02: &bands[0],
            b03: &bands[1],
            b04: &bands[2],
            b8a: &bands[3],
            b11: &bands[4],
            b12: &bands[5],
            b10: &bands[6],
            b07: &bands[7],
            b08: &bands[8],
        },
        rows,
        cols,
        radio,
        compat,
    );
    drop(bands);

    let xml = std::fs::read_to_string(&safe.tile).map_err(|e| gdal_err(&safe.tile, e))?;
    let (zen, azi) = mean_sun_angle(&xml)?;
    let (vza, vaa, view_missing) = view_angles_20m(safe, desc)?;

    let mut dem20: Option<Vec<f32>> = None;
    if let Some(d) = dem {
        let mut v = match d {
            FmaskDem::File(path) => dem_on_grid(path, safe, Some(20.0), Some(desc), n_jobs)?,
            FmaskDem::Array(a) if a.len() == n => decimate_f32(a, nr, nc),
            FmaskDem::Array(a) if a.len() == n20 => a.to_vec(),
            FmaskDem::Array(a) => {
                return Err(format!(
                    "dem has {} values, expected the 20 m grid ({n20}) or the 10 m one ({n})",
                    a.len()
                ));
            }
        };
        for x in v.iter_mut() {
            if *x == DEM_NODATA as f32 {
                *x = f32::NAN;
            }
        }
        dem20 = Some(v);
    }
    let slope_aspect = dem20.as_ref().map(|d| run::slope_aspect_deg(d, rows, cols, 20.0, threads(n_jobs) as i32));
    let gswo20 = match gswo {
        Some(path) => Some(gswo_on_grid(path, safe, desc, n_jobs)?),
        None => None,
    };

    let inp = RunInput {
        rows,
        cols,
        toa: zeit_core::fmask::detect::Toa20 {
            blue: &toa.blue,
            green: &toa.green,
            red: &toa.red,
            nir: &toa.nir,
            swir1: &toa.swir1,
            swir2: &toa.swir2,
            cirrus: &toa.cirrus,
        },
        b7: &toa.b7,
        b8: &toa.b8,
        satu_blue: &toa.satu_blue,
        satu_green: &toa.satu_green,
        satu_red: &toa.satu_red,
        dem: dem20.as_deref(),
        slope: slope_aspect.as_ref().map(|s| s.0.as_slice()),
        aspect: slope_aspect.as_ref().map(|s| s.1.as_slice()),
        gswo: gswo20.as_deref(),
        sun_zenith: zen,
        sun_azimuth: azi,
        vza: &vza,
        vaa: &vaa,
        resolution: 20.0,
    };
    let cfg = RunConfig { params: FmaskParameters::default(), compat, keep_steps: false };
    let r = run::run(&inp, &cfg);
    let flags = r.to_qai(2, 20.0, 300.0, threads(n_jobs) as i32);

    let observed = r.mask.iter().filter(|&&m| m != 0).count();
    let n_obs = observed.max(1) as f64;
    let frac = |a: &[u8]| a.iter().zip(&r.mask).filter(|(a, m)| **a != 0 && **m != 0).count() as f64 / n_obs;
    let info = FmaskInfo {
        sum_clr: r.sum_clr,
        few_clear: r.few_clear,
        match_skipped: r.match_skipped,
        clr_pct: r.clr_pct,
        observed: observed as f64 / r.mask.len().max(1) as f64,
        cloud: frac(&r.pcloud),
        shadow: frac(&r.pshadow),
        snow: frac(&r.psnow),
        water: frac(&r.water),
        route: if new { "baseline04" } else { "legacy" },
        sun_zenith: zen,
        sun_azimuth: azi,
        view_missing,
    };
    Ok((flags, info))
}

/// `a[::2, ::2]` of an `f32` image of `nr x nc`.
fn decimate_f32(a: &[f32], nr: usize, nc: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity((nr / 2) * (nc / 2));
    for r in (0..nr).step_by(2) {
        out.extend(a[r * nc..(r + 1) * nc].iter().step_by(2));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(p: &Path) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"").unwrap();
    }

    #[test]
    fn safe_discovery() {
        let dir = std::env::temp_dir().join(format!("janus_level2_safe_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let safe = dir.join("S2B_TEST.SAFE");
        touch(&safe.join("MTD_MSIL1C.xml"));
        let g = safe.join("GRANULE/L1C_T23LKC_A000000_20230728T132238");
        touch(&g.join("MTD_TL.xml"));
        for name in BAND_NAMES {
            // the TIFF copies of a product keep the band names; a JPEG 2000 file wins over a TIFF of the same band
            touch(&g.join(format!("IMG_DATA/T23LKC_20230728T132239_B{name}.jp2")));
        }
        touch(&g.join("IMG_DATA/T23LKC_20230728T132239_B02.tif"));
        let s = Safe::open(&safe).unwrap();
        assert_eq!(s.bands.len(), 13);
        assert_eq!(s.granule_name(), "L1C_T23LKC_A000000_20230728T132238");
        assert!(s.bands[8].to_string_lossy().ends_with("_B8A.jp2"));
        assert!(s.bands[1].to_string_lossy().ends_with("_B02.jp2"));
        // a band without a file is named in the error
        std::fs::remove_file(g.join("IMG_DATA/T23LKC_20230728T132239_B12.jp2")).unwrap();
        let e = Safe::open(&safe).unwrap_err();
        assert!(e.contains("*_B12.jp2"), "{e}");
        // two granules are not one product
        touch(&safe.join("GRANULE/other/MTD_TL.xml"));
        assert!(Safe::open(&safe).unwrap_err().contains("not a Sentinel-2 L1C"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sun_angle_of_the_tile_metadata() {
        let xml = r#"<?xml version="1.0"?>
<n1:Level-1C_Tile_ID xmlns:n1="https://psd-14.sentinel2.eo.esa.int/PSD/S2_PDI_Level-1C_Tile_Metadata.xsd">
  <n1:Geometric_Info><Tile_Angles>
    <Sun_Angles_Grid><Zenith><VALUES>1 2</VALUES></Zenith></Sun_Angles_Grid>
    <Mean_Sun_Angle>
      <ZENITH_ANGLE unit="deg">47.5326</ZENITH_ANGLE>
      <AZIMUTH_ANGLE unit="deg">  35.8153 </AZIMUTH_ANGLE>
    </Mean_Sun_Angle>
  </Tile_Angles></n1:Geometric_Info>
</n1:Level-1C_Tile_ID>"#;
        assert_eq!(mean_sun_angle(xml).unwrap(), (47.5326, 35.8153));
        assert!(mean_sun_angle("<a><b>1</b></a>").unwrap_err().contains("no Mean_Sun_Angle"));
        assert!(mean_sun_angle("<a><Mean_Sun_Angle><ZENITH_ANGLE>x</ZENITH_ANGLE></Mean_Sun_Angle></a>").is_err());
    }

    #[test]
    fn numpy_mean_of_float32() {
        // the sum is the first value plus the pairwise sum of the rest, as numpy's add.reduce does
        let a: Vec<f32> = (0..1000).map(|i| (i % 7) as f32 * 0.1 + 0.05).collect();
        let s = numpy_sum_f32(&a);
        let exact: f64 = a.iter().map(|&v| f64::from(v)).sum();
        assert!((f64::from(s) - exact).abs() < 1e-3, "{s} vs {exact}");
        assert_eq!(numpy_sum_f32(&[]), 0.0);
        assert_eq!(numpy_sum_f32(&[2.5]), 2.5);
    }

    #[test]
    fn decimation_takes_every_step_th_value() {
        let a: Vec<u16> = (0..36).collect();
        assert_eq!(&*decimate(&a, 6, 6, 2), &[0, 2, 4, 12, 14, 16, 24, 26, 28]);
        assert_eq!(&*decimate(&a, 6, 6, 6), &[0]);
        assert_eq!(&*decimate(&a, 6, 6, 1), &a[..]);
    }
}
