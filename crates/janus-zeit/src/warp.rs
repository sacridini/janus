//! Reproject, resample, clip, align to a grid (IDEIAS phase 43): gdalwarp's own code (GDALWarp with the command
//! line's options, so a result is the command's bit for bit) on a map or a series' files, one after the other on
//! the processing threads (-multi -wo NUM_THREADS); to another CRS, pixel size or a layer's grid (its CRS, origin,
//! pixel and extent), on an extent or cut by a vector layer's polygons; written as COGs or GeoTIFFs, or virtual
//! (warped VRTs: nothing written, ready at once).

use std::ffi::{CString, c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, Ordering};

use gdal::Dataset;

/// gdalwarp's resampling methods offered (-r).
pub const RESAMPLING: [&str; 8] = ["near", "bilinear", "cubic", "average", "mode", "min", "max", "med"];

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Format {
    #[default]
    Cog,
    GTiff,
    /// warped VRTs: the pixels computed when read
    Vrt,
}

impl Format {
    pub fn extension(self) -> &'static str {
        if self == Format::Vrt { "vrt" } else { "tif" }
    }
}

/// A grid to warp onto: its CRS, geotransform (north up) and size.
#[derive(Clone, Debug)]
pub struct Grid {
    pub wkt: String,
    pub gt: [f64; 6],
    pub w: usize,
    pub h: usize,
}

#[derive(Clone, Debug, Default)]
pub struct WarpOptions {
    /// the target CRS (EPSG:4326, a WKT...; None: the source's)
    pub t_srs: Option<String>,
    /// another layer's grid: its CRS, origin, pixel and extent (t_srs, pixel and extent then unused)
    pub grid: Option<Grid>,
    /// the pixel size in the target CRS's units (None: gdalwarp's)
    pub pixel: Option<(f64, f64)>,
    /// the pixels' edges on multiples of the pixel size (-tap)
    pub tap: bool,
    /// xmin, ymin, xmax, ymax in a CRS (-te, -te_srs)
    pub extent: Option<([f64; 4], String)>,
    /// a vector file and its layer whose polygons cut the result (-cutline), the result cropped to them
    pub cutline: Option<(String, String)>,
    pub resampling: String,
    pub dst_nodata: Option<f64>,
    /// Byte, Int16, UInt16, Int32, Float32... (None: the source's)
    pub data_type: Option<String>,
    pub format: Format,
    pub threads: usize,
}

/// A number as GDAL reads it back exactly.
fn num(v: f64) -> String {
    format!("{v:?}")
}

impl WarpOptions {
    /// gdalwarp's arguments (the command's, less its input and output).
    pub fn args(&self) -> Result<Vec<String>, String> {
        let mut a: Vec<String> = vec![];
        let mut push = |xs: &[&str]| a.extend(xs.iter().map(|s| s.to_string()));
        if let Some(g) = &self.grid {
            if g.gt[2] != 0.0 || g.gt[4] != 0.0 {
                return Err("a rotated grid cannot be the target".into());
            }
            let (x0, x1) = (g.gt[0], g.gt[0] + g.gt[1] * g.w as f64);
            let (y0, y1) = (g.gt[3], g.gt[3] + g.gt[5] * g.h as f64);
            push(&["-t_srs", &g.wkt]);
            push(&["-te", &num(x0.min(x1)), &num(y0.min(y1)), &num(x0.max(x1)), &num(y0.max(y1))]);
            push(&["-ts", &g.w.to_string(), &g.h.to_string()]);
        } else {
            if let Some(s) = &self.t_srs {
                push(&["-t_srs", s]);
            }
            if let Some((x, y)) = self.pixel {
                push(&["-tr", &num(x), &num(y)]);
                if self.tap {
                    push(&["-tap"]);
                }
            }
            if let Some((e, srs)) = &self.extent {
                push(&["-te", &num(e[0]), &num(e[1]), &num(e[2]), &num(e[3]), "-te_srs", srs]);
            }
        }
        if let Some((file, layer)) = &self.cutline {
            push(&["-cutline", file]);
            if !layer.is_empty() {
                push(&["-cl", layer]);
            }
            if self.grid.is_none() {
                push(&["-crop_to_cutline"]);
            }
        }
        push(&["-r", if self.resampling.is_empty() { "near" } else { &self.resampling }]);
        if let Some(v) = self.dst_nodata {
            push(&["-dstnodata", &num(v)]);
        }
        if let Some(t) = &self.data_type {
            push(&["-ot", t]);
        }
        let threads = self.threads.max(1).to_string();
        match self.format {
            Format::Cog => push(&[
                "-of",
                "COG",
                "-co",
                "COMPRESS=DEFLATE",
                "-co",
                "PREDICTOR=YES",
                "-co",
                "BIGTIFF=IF_SAFER",
                "-co",
                &format!("NUM_THREADS={threads}"),
            ]),
            Format::GTiff => push(&[
                "-of",
                "GTiff",
                "-co",
                "TILED=YES",
                "-co",
                "COMPRESS=DEFLATE",
                "-co",
                "PREDICTOR=2",
                "-co",
                "BIGTIFF=IF_SAFER",
                "-co",
                &format!("NUM_THREADS={threads}"),
            ]),
            Format::Vrt => push(&["-of", "VRT"]),
        }
        if self.format != Format::Vrt {
            push(&["-multi", "-wo", &format!("NUM_THREADS={threads}")]);
        }
        push(&["-overwrite"]);
        Ok(a)
    }
}

/// The progress: a function of the fraction done, false to stop.
pub(crate) struct Progress<'a> {
    pub(crate) f: &'a dyn Fn(f64) -> bool,
}

pub(crate) unsafe extern "C" fn on_progress(done: f64, _msg: *const c_char, arg: *mut c_void) -> c_int {
    // SAFETY: arg is the Progress given to GDALWarpAppOptionsSetProgress, alive during GDALWarp.
    let p = unsafe { &*(arg as *const Progress) };
    c_int::from((p.f)(done))
}

/// Warps `src` into `dst` (replaced); `progress` returns false to stop.
pub fn warp(src: &str, dst: &str, opts: &WarpOptions, progress: &dyn Fn(f64) -> bool) -> Result<(), String> {
    let args = opts.args()?;
    let ds = Dataset::open(src).map_err(|e| format!("{src}: {e}"))?;
    let c = |s: &str| CString::new(s).map_err(|_| format!("{s}: a NUL in it"));
    let dst_c = c(dst)?;
    let p = Progress { f: progress };
    // SAFETY: GDAL's C API: the argument list and options made and freed here, the source alive throughout.
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
        gdal_sys::GDALWarpAppOptionsSetProgress(o, Some(on_progress), &p as *const Progress as *mut c_void);
        let mut src_h = [ds.c_dataset()];
        let mut usage = 0;
        let out = gdal_sys::GDALWarp(dst_c.as_ptr(), null_mut(), 1, src_h.as_mut_ptr(), o, &mut usage);
        gdal_sys::GDALWarpAppOptionsFree(o);
        if out.is_null() {
            let e = crate::polygonize::last_error();
            let _ = std::fs::remove_file(dst);
            return Err(if e.is_empty() { "gdalwarp failed".into() } else { e });
        }
        // the classes' names (GDALWarp keeps a colour table, not the names)
        for b in 1..=gdal_sys::GDALGetRasterCount(src_h[0]).min(gdal_sys::GDALGetRasterCount(out)) {
            let names = gdal_sys::GDALGetRasterCategoryNames(gdal_sys::GDALGetRasterBand(src_h[0], b));
            if gdal_sys::CSLCount(names) > 0 {
                gdal_sys::GDALSetRasterCategoryNames(gdal_sys::GDALGetRasterBand(out, b), names);
            }
        }
        gdal_sys::GDALClose(out);
    }
    Ok(())
}

/// Warps each file into `out_dir` under its own name (.tif, or .vrt when virtual), in turn; the files written.
pub fn warp_files(
    files: &[String],
    out_dir: &Path,
    opts: &WarpOptions,
    progress: &dyn Fn(f64, &str),
    cancel: &AtomicBool,
) -> Result<Vec<String>, String> {
    let mut names: Vec<PathBuf> = Vec::with_capacity(files.len());
    for (i, f) in files.iter().enumerate() {
        let stem = Path::new(f).file_stem().map_or(format!("map_{i}"), |s| s.to_string_lossy().into_owned());
        let mut p = out_dir.join(format!("{stem}.{}", opts.format.extension()));
        // two files of one name (from different folders): the second numbered
        if names.contains(&p) {
            p = out_dir.join(format!("{stem}_{i}.{}", opts.format.extension()));
        }
        names.push(p);
    }
    let n = files.len().max(1) as f64;
    for (i, (f, out)) in files.iter().zip(&names).enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let label = format!("{} of {}: {}", i + 1, files.len(), out.file_name().unwrap_or_default().to_string_lossy());
        progress(i as f64 / n, &label);
        let step = |d: f64| {
            progress((i as f64 + d) / n, &label);
            !cancel.load(Ordering::Relaxed)
        };
        warp(f, &out.to_string_lossy(), opts, &step)
            .map_err(|e| if cancel.load(Ordering::Relaxed) { "cancelled".to_string() } else { format!("{f}: {e}") })?;
    }
    progress(1.0, "done");
    Ok(names.iter().map(|p| p.to_string_lossy().into_owned()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gdal::spatial_ref::SpatialRef;

    fn source(path: &str, w: usize, h: usize) -> Vec<f32> {
        let data: Vec<f32> = (0..w * h).map(|i| (i % 97) as f32 + (i / w) as f32 * 0.5).collect();
        let drv = gdal::DriverManager::get_driver_by_name("GTiff").unwrap();
        let mut ds = drv.create_with_band_type::<f32, _>(path, w, h, 1).unwrap();
        ds.set_geo_transform(&[600000.0, 30.0, 0.0, 7500000.0, 0.0, -30.0]).unwrap();
        ds.set_projection(&SpatialRef::from_epsg(32723).unwrap().to_wkt().unwrap()).unwrap();
        ds.rasterband(1).unwrap().write((0, 0), (w, h), &mut gdal::raster::Buffer::new((w, h), data.clone())).unwrap();
        data
    }

    fn read(path: &str) -> (Vec<f32>, usize, usize, [f64; 6], String) {
        let ds = Dataset::open(path).unwrap();
        let (w, h) = ds.raster_size();
        let v = ds.rasterband(1).unwrap().read_as::<f32>((0, 0), (w, h), (w, h), None).unwrap().into_shape_and_vec().1;
        (v, w, h, ds.geo_transform().unwrap(), ds.projection())
    }

    /// Clipped on the source's grid with near: the pixels of the window, unchanged; onto a grid: its size.
    #[test]
    fn clip_and_grid() {
        let (w, h) = (60, 40);
        let src = "/vsimem/warp_src.tif";
        let data = source(src, w, h);
        let wkt = SpatialRef::from_epsg(32723).unwrap().to_wkt().unwrap();
        // pixels 10..30 x 5..25
        let e = [600000.0 + 300.0, 7500000.0 - 750.0, 600000.0 + 900.0, 7500000.0 - 150.0];
        let opts = WarpOptions {
            extent: Some((e, wkt.clone())),
            pixel: Some((30.0, 30.0)),
            format: Format::GTiff,
            threads: 2,
            ..Default::default()
        };
        let none = AtomicBool::new(false);
        let dir = std::env::temp_dir().join(format!("janus_warp_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = warp_files(&[src.to_string()], &dir, &opts, &|_, _| {}, &none).unwrap();
        let (v, ow, oh, gt, _) = read(&out[0]);
        assert_eq!((ow, oh), (20, 20));
        assert_eq!(gt[0], e[0]);
        for y in 0..20 {
            for x in 0..20 {
                assert_eq!(v[y * 20 + x], data[(y + 5) * w + x + 10]);
            }
        }
        // onto a grid in longitudes and latitudes, virtual
        let g = Grid {
            wkt: SpatialRef::from_epsg(4326).unwrap().to_wkt().unwrap(),
            gt: [-44.03, 0.0005, 0.0, -22.59, 0.0, -0.0005],
            w: 30,
            h: 20,
        };
        let opts = WarpOptions { grid: Some(g.clone()), resampling: "bilinear".into(), format: Format::Vrt, ..opts };
        let out = warp_files(&[src.to_string()], &dir, &opts, &|_, _| {}, &none).unwrap();
        assert!(out[0].ends_with(".vrt"));
        let (_, ow, oh, gt, wkt) = read(&out[0]);
        assert_eq!((ow, oh), (g.w, g.h));
        assert!(gt.iter().zip(&g.gt).all(|(a, b)| (a - b).abs() < 1e-12), "{gt:?}");
        assert!(SpatialRef::from_wkt(&wkt).unwrap().is_geographic());
        // stopped
        let stop = AtomicBool::new(true);
        assert_eq!(warp_files(&[src.to_string()], &dir, &opts, &|_, _| {}, &stop).unwrap_err(), "cancelled");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A class map's names kept in every format.
    #[test]
    fn class_names_kept() {
        let src = "/vsimem/warp_classes.tif";
        source(src, 20, 20);
        let names_of = |path: &str| -> Vec<String> {
            let ds = Dataset::open(path).unwrap();
            // a GeoTIFF reads its .aux.xml (the names) with its georeference, not for the names alone
            let _ = ds.geo_transform();
            // SAFETY: the band of a dataset alive here; the list owned by it, read before it closes.
            unsafe {
                let l = gdal_sys::GDALGetRasterCategoryNames(gdal_sys::GDALGetRasterBand(ds.c_dataset(), 1));
                (0..gdal_sys::CSLCount(l))
                    .map(|i| std::ffi::CStr::from_ptr(*l.offset(i as isize)).to_string_lossy().into_owned())
                    .collect()
            }
        };
        {
            let ds = Dataset::open_ex(
                src,
                gdal::DatasetOptions { open_flags: gdal::GdalOpenFlags::GDAL_OF_UPDATE, ..Default::default() },
            )
            .unwrap();
            let names: Vec<CString> = ["none", "forest", "water"].iter().map(|s| CString::new(*s).unwrap()).collect();
            // SAFETY: a NULL-ended list of strings alive during the call, which copies them.
            unsafe {
                let mut list: Vec<*mut c_char> = names.iter().map(|s| s.as_ptr() as *mut c_char).collect();
                list.push(null_mut());
                gdal_sys::GDALSetRasterCategoryNames(gdal_sys::GDALGetRasterBand(ds.c_dataset(), 1), list.as_mut_ptr());
            }
        }
        assert_eq!(names_of(src), ["none", "forest", "water"]);
        let none = AtomicBool::new(false);
        let dir = std::env::temp_dir().join(format!("janus_warp_names_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for format in [Format::Cog, Format::GTiff, Format::Vrt] {
            let opts = WarpOptions { t_srs: Some("EPSG:4326".into()), format, threads: 2, ..Default::default() };
            let out = warp_files(&[src.to_string()], &dir, &opts, &|_, _| {}, &none).unwrap();
            assert_eq!(names_of(&out[0]), ["none", "forest", "water"], "{format:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn arguments() {
        let o = WarpOptions {
            t_srs: Some("EPSG:4326".into()),
            pixel: Some((0.001, 0.001)),
            resampling: "mode".into(),
            dst_nodata: Some(0.0),
            threads: 8,
            ..Default::default()
        };
        let a = o.args().unwrap().join(" ");
        assert!(a.starts_with("-t_srs EPSG:4326 -tr 0.001 0.001 -r mode -dstnodata 0.0 -of COG"), "{a}");
        assert!(a.ends_with("-multi -wo NUM_THREADS=8 -overwrite"), "{a}");
    }
}
