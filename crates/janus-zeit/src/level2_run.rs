//! The run of Zeit's Level 2 correction of a Sentinel-2 L1C scene and its products (`zeit.level2`, FORCE's `force-l2ps`): the
//! entry [`level2`] reads the inputs ([`crate::level2`]), runs zeit-core's `l2scene::level2_scene` as zeit-py's `l2scene.run`
//! does, with the resolution merge as its `ToaHook` ([`toa_merge`], `_toa_merge`), and [`Level2Output::save`] writes the
//! products to a folder as Zeit's `ds.zeit.save(folder)` does (one GeoTIFF per variable).
//!
//! Mapping to `zeit/_level2.py`: [`level2`] is `level2` and `_level2_arrays` without `flags=`, `geography=`, `sun=`,
//! `brdf_table=`, `bands=`, `dem_array=` and `reference=`; [`toa_merge`] is `_toa_merge`; [`Level2Output`] is the Dataset
//! `level2` returns and [`Level2Attrs`] its `attrs`.

use std::path::{Path, PathBuf};
use std::time::Instant;

use gdal::{DriverManager, Metadata};
use gdal::raster::{Buffer, RasterCreationOptions};
use gdal::spatial_ref::SpatialRef;
use zeit_core::atmo::L2Params;
use zeit_core::gas::Toa;
use zeit_core::l2::L2Error;
use zeit_core::l2scene::{self, SceneParams};
use zeit_core::resmerge::{self, NT, Scene};
use zeit_core::s2l1c::NB;
use zeit_core::sunview::GeoTable;

use crate::level2::{self, DEM_NODATA, FmaskDem, Safe};

/// The bands of `boa` in the order of the product (`BOA_BANDS`).
pub const BOA_BANDS: [&str; 10] = ["BLUE", "GREEN", "RED", "REDEDGE1", "REDEDGE2", "REDEDGE3", "BROADNIR", "NIR", "SWIR1", "SWIR2"];
/// The no-data value of `boa`.
pub const BOA_NODATA: i16 = -9999;
/// The no-data value of the TOA reflectance the merge writes.
pub const TOA_NODATA: i16 = -9999;
/// The `provenance` attribute (`_PROVENANCE`).
pub const PROVENANCE: &str = "Sentinel-2 L1C to Level 2: the atmospheric correction of FORCE's force-l2ps (Frantz 2019, Remote Sensing 11:1124; FORCE 3.11.00, David Frantz, GPL-3.0-or-later) ported to the Zeit core, with the cloud, shadow, snow and water flags of Zeit's Fmask 4.7 port (Qiu, Zhu and Woodcock 2019, Remote Sensing of Environment 231:111205) unless flags were given or the Fmask was switched off";

/// The names of the domains of the nine bands of the merge: the ones that predict (green, red, broad NIR) and the six
/// predicted (`_MERGE_ROLES`).
const MERGE_DOMAINS: [&str; 3 + NT] = ["GREEN", "RED", "BROADNIR", "REDEDGE1", "REDEDGE2", "REDEDGE3", "NIR", "SWIR1", "SWIR2"];

/// FORCE's resolution merge (`RES_MERGE`): the six 20 m bands predicted at 10 m.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Merge {
    Regression,
    Improphe,
}

impl Merge {
    pub fn name(self) -> &'static str {
        match self {
            Merge::Regression => "regression",
            Merge::Improphe => "improphe",
        }
    }

    /// `None`, `"regression"` or `"improphe"`; anything else is an error as in `_check_merge_coreg`.
    pub fn parse(s: Option<&str>) -> Result<Option<Merge>, String> {
        match s {
            None => Ok(None),
            Some("regression") => Ok(Some(Merge::Regression)),
            Some("improphe") => Ok(Some(Merge::Improphe)),
            Some(o) => Err(format!("merge must be None, 'regression' or 'improphe', got {o:?}")),
        }
    }
}

/// The options of `zeit.level2` (without `flags`, `reference`, `coreg_band` and `coreg_ref_band`).
// TODO: `reference=` (the coregistration of the TOA before the merge, `_toa_coregister`) is not ported yet.
#[derive(Clone, Debug)]
pub struct Level2Options {
    /// The digital elevation model (a raster in metres of any grid and CRS).
    pub dem: Option<PathBuf>,
    pub brdf: bool,
    /// The topographic correction; needs `dem`.
    pub topography: bool,
    pub aod: bool,
    pub adjacency: bool,
    pub multiple_scattering: bool,
    pub erase_cloud: bool,
    /// Run Zeit's Fmask for the cloud, shadow, snow and water flags (False: no cloud detection).
    pub fmask: bool,
    pub gswo: Option<PathBuf>,
    pub fmask_compat: bool,
    /// The resampling of the DEM onto the grid of the scene: `bilinear`, `nearest`, `cubic`...
    pub dem_resampling: String,
    pub n_jobs: usize,
    pub merge: Option<Merge>,
}

impl Default for Level2Options {
    fn default() -> Self {
        Level2Options {
            dem: None,
            brdf: true,
            topography: true,
            aod: true,
            adjacency: true,
            multiple_scattering: true,
            erase_cloud: false,
            fmask: true,
            gswo: None,
            fmask_compat: false,
            dem_resampling: "bilinear".into(),
            n_jobs: 8,
            merge: None,
        }
    }
}

/// An attribute of the Dataset in the types `_plain` serializes.
#[derive(Clone, Debug, PartialEq)]
pub enum Attr {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<Attr>),
}

/// The `attrs` of the Dataset `zeit.level2` returns.
#[derive(Clone, Debug)]
pub struct Level2Attrs {
    /// `YYYY-MM-DDTHH:MM:SSZ`, UTC.
    pub datetime: String,
    pub sensor: String,
    /// `str(baseline)`: `None` as text when the metadata has none.
    pub baseline: String,
    /// The optical depth of the scene in B03 (NaN if there is none).
    pub aod_green: f64,
    /// False when the fallback spectrum was used.
    pub aod_map: bool,
    /// The mean of the `wvp` product in cm (NaN if none).
    pub water_vapor_cm: f64,
    /// Valid water, shadow and vegetation dark targets.
    pub n_targets: [i32; 3],
    /// The time of the core.
    pub core_seconds: f64,
    pub merge: Option<&'static str>,
    /// The Fmask's summary, the `fmask_*` keys without the prefix (empty when it did not run).
    pub fmask: Vec<(String, Attr)>,
    pub provenance: &'static str,
}

impl Level2Attrs {
    /// Every attribute as `(key, value)` in the order of `ds.attrs`.
    pub fn pairs(&self) -> Vec<(String, Attr)> {
        let mut v = vec![
            ("datetime".to_string(), Attr::Str(self.datetime.clone())),
            ("sensor".into(), Attr::Str(self.sensor.clone())),
            ("baseline".into(), Attr::Str(self.baseline.clone())),
            ("aod_green".into(), Attr::Float(self.aod_green)),
            ("aod_map".into(), Attr::Bool(self.aod_map)),
            ("water_vapor_cm".into(), Attr::Float(self.water_vapor_cm)),
            ("n_targets".into(), Attr::List(self.n_targets.iter().map(|&n| Attr::Int(i64::from(n))).collect())),
            ("core_seconds".into(), Attr::Float(self.core_seconds)),
        ];
        if let Some(m) = self.merge {
            v.push(("merge".into(), Attr::Str(m.into())));
        }
        for (k, a) in &self.fmask {
            v.push((format!("fmask_{k}"), a.clone()));
        }
        v.push(("provenance".into(), Attr::Str(self.provenance.into())));
        v
    }
}

/// The Dataset `zeit.level2` returns: the products on the grid of the scene (row by row; `boa` band by band) and its
/// georeferencing and attributes.
#[derive(Clone, Debug)]
pub struct Level2Output {
    pub nrows: usize,
    pub ncols: usize,
    pub epsg: u32,
    pub ulx: f64,
    pub uly: f64,
    pub res: f64,
    /// `10 * nrows * ncols`, in the order of [`BOA_BANDS`], reflectance x 10000, [`BOA_NODATA`] where there is none.
    pub boa: Vec<i16>,
    pub qai: Vec<i16>,
    pub dst: Option<Vec<i16>>,
    pub vzn: Option<Vec<i16>>,
    pub hot: Option<Vec<i16>>,
    pub aod: Option<Vec<i16>>,
    pub wvp: Option<Vec<i16>>,
    pub attrs: Level2Attrs,
}

impl Level2Output {
    /// The GDAL geotransform of the scene (`Affine(res, 0, ulx, 0, -res, uly)`).
    pub fn geo_transform(&self) -> [f64; 6] {
        [self.ulx, self.res, 0.0, self.uly, 0.0, -self.res]
    }

    /// The 2-D products by name in the order of the Dataset (`qai`, `dst`, `vzn`, `hot`, `aod`, `wvp`).
    pub fn planes(&self) -> Vec<(&'static str, &[i16])> {
        let mut v: Vec<(&'static str, &[i16])> = vec![("qai", &self.qai)];
        for (k, p) in [("dst", &self.dst), ("vzn", &self.vzn), ("hot", &self.hot), ("aod", &self.aod), ("wvp", &self.wvp)] {
            if let Some(p) = p {
                v.push((k, p));
            }
        }
        v
    }

    /// Writes the products to `folder` (made if missing) as `ds.zeit.save(folder)` does: `boa.tif` (10 bands named as
    /// [`BOA_BANDS`], no data -9999) and one single band GeoTIFF per other product, all `int16`, in the CRS `EPSG:<epsg>`,
    /// deflate-compressed with predictor 2 (tiles of 256 when the image is larger than that, BigTIFF if needed). The
    /// attributes are not written, as Zeit's save does not. Returns the files in the order written.
    pub fn save(&self, folder: &Path) -> Result<Vec<PathBuf>, String> {
        std::fs::create_dir_all(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
        let n = self.nrows * self.ncols;
        let mut out = vec![];
        let boa: Vec<&[i16]> = self.boa.chunks(n.max(1)).collect();
        let f = folder.join("boa.tif");
        self.write_tif(&f, &boa, Some(&BOA_BANDS), Some(f64::from(BOA_NODATA)))?;
        out.push(f);
        for (k, p) in self.planes() {
            let f = folder.join(format!("{k}.tif"));
            self.write_tif(&f, &[p], None, None)?;
            out.push(f);
        }
        Ok(out)
    }

    fn write_tif(&self, path: &Path, bands: &[&[i16]], names: Option<&[&str]>, nodata: Option<f64>) -> Result<(), String> {
        let e = |e: gdal::errors::GdalError| format!("{}: {e}", path.display());
        let (w, h) = (self.ncols, self.nrows);
        let mut opts = vec!["COMPRESS=DEFLATE".to_string(), "PREDICTOR=2".into(), "BIGTIFF=IF_SAFER".into()];
        if h > 256 || w > 256 {
            opts.extend(["TILED=YES".to_string(), "BLOCKXSIZE=256".into(), "BLOCKYSIZE=256".into()]);
        }
        let co = RasterCreationOptions::from_iter(opts);
        let drv = DriverManager::get_driver_by_name("GTiff").map_err(|x| x.to_string())?;
        let mut ds = drv.create_with_band_type_with_options::<i16, _>(path, w, h, bands.len(), &co).map_err(e)?;
        ds.set_geo_transform(&self.geo_transform()).map_err(e)?;
        let srs = SpatialRef::from_epsg(self.epsg).map_err(|x| x.to_string())?;
        ds.set_spatial_ref(&srs).map_err(e)?;
        for (k, data) in bands.iter().enumerate() {
            let mut band = ds.rasterband(k + 1).map_err(e)?;
            if let Some(nd) = nodata {
                band.set_no_data_value(Some(nd)).map_err(e)?;
            }
            if let Some(names) = names {
                band.set_description(names[k]).map_err(e)?;
            }
            let mut buf = Buffer::new((w, h), data.to_vec());
            band.write((0, 0), (w, h), &mut buf).map_err(e)?;
        }
        ds.flush_cache().map_err(e)?;
        Ok(())
    }
}

// --- the merge ----------------------------------------------------------------------------------------------------

/// The TOA with the six 20 m bands predicted at 10 m (FORCE's `resolution_merge`, `zeit.sentinel2_resmerge` with
/// `force=True, scale=10000.0, nodata=None`, its arithmetic: `_toa_merge`): the pixels that are off (bit 0 of the QAI) are
/// -9999 and left out; the ones with a cloud, a shadow or off are not predicted (`clear` is `!off && cloud == 0 && !shadow`).
/// With `force=True` the regression sweeps the image in order on one thread, whatever `n_jobs`.
pub fn toa_merge(toa: &mut Toa, qai: &[i16], method: Merge, n_jobs: i32) -> Result<(), L2Error> {
    let (nx, ny) = (toa.grid.nx, toa.grid.ny);
    let n = nx * ny;
    if qai.len() != n || toa.bands.iter().any(|b| b.len() != n) {
        return Err(L2Error::Other(format!("the merge needs the TOA and the QAI of {nx} x {ny} pixels")));
    }
    let mut idx = [0usize; 3 + NT];
    for (i, dom) in MERGE_DOMAINS.iter().enumerate() {
        idx[i] = toa.find_domain(dom).ok_or_else(|| L2Error::Other(format!("the merge needs the {dom} band")))?;
    }
    let off: Vec<bool> = qai.iter().map(|&q| q & 1 != 0).collect();
    let bad: Vec<bool> = qai.iter().map(|&q| (q & 1) != 0 || ((q >> 1) & 3) > 0 || ((q >> 3) & 1) != 0).collect();
    // the cube: the TOA as float32, NaN where the pixel is off
    let plane = |b: usize| -> Vec<f32> {
        toa.bands[b].iter().zip(&off).map(|(&v, &o)| if o { f32::NAN } else { f32::from(v) }).collect()
    };
    let (green, red, bnir) = (plane(idx[0]), plane(idx[1]), plane(idx[2]));
    let mut targets = Vec::with_capacity(NT * n);
    for &b in &idx[3..] {
        targets.extend(plane(b));
    }
    // nothing is missing in a band but the off pixels (the cube's NaN), which `off` of the merge lists
    let sc = Scene { nx, ny, green: &green, red: &red, bnir: &bnir, targets: &targets, off: &off, bad: &bad };
    let merged = match method {
        Merge::Regression => resmerge::regression(&sc, 10000.0, true, n_jobs),
        Merge::Improphe => resmerge::improphe(&sc, 10000.0, true, n_jobs),
    };
    drop((green, red, bnir, targets));
    for (k, &b) in idx[3..].iter().enumerate() {
        for ((o, &v), &is_off) in toa.bands[b].iter_mut().zip(&merged[k * n..(k + 1) * n]).zip(&off) {
            *o = if is_off {
                TOA_NODATA
            } else if v.is_nan() {
                TOA_NODATA
            } else {
                v.clamp(-32768.0, 32767.0) as i16
            };
        }
    }
    Ok(())
}

// --- the run -------------------------------------------------------------------------------------------------------

/// The DEM's resampling as GDAL names it (`nearest` is `near`, `cubic_spline` is `cubicspline`).
fn gdal_method(name: &str) -> String {
    match name {
        "nearest" => "near".into(),
        "cubic_spline" => "cubicspline".into(),
        o => o.into(),
    }
}

/// The Fmask's summary as `_plain` serializes it: the keys of the dict `fmask_flags` returns, in its order (None is left out).
fn fmask_attrs(
    info: &level2::FmaskInfo,
    baseline: &Option<String>,
    grid: (usize, usize),
    dem: bool,
    gswo: bool,
    compat: bool,
) -> Vec<(String, Attr)> {
    let f = |k: &str, a: Attr| (k.to_string(), a);
    let mut v = vec![
        f("sum_clr", Attr::Int(info.sum_clr as i64)),
        f("few_clear", Attr::Bool(info.few_clear)),
        f("match_skipped", Attr::Bool(info.match_skipped)),
        f("clr_pct", Attr::Float(info.clr_pct)),
        f("observed", Attr::Float(info.observed)),
        f("cloud", Attr::Float(info.cloud)),
        f("shadow", Attr::Float(info.shadow)),
        f("snow", Attr::Float(info.snow)),
        f("water", Attr::Float(info.water)),
    ];
    if let Some(b) = baseline {
        v.push(f("baseline", Attr::Str(b.clone())));
    }
    v.extend([
        f("route", Attr::Str(info.route.into())),
        f("sun_zenith", Attr::Float(info.sun_zenith)),
        f("sun_azimuth", Attr::Float(info.sun_azimuth)),
        f("view_band", Attr::Str("B8A".into())),
        f("view_missing", Attr::Float(info.view_missing)),
        f("dem", Attr::Bool(dem)),
        f("gswo", Attr::Bool(gswo)),
        f("compat", Attr::Bool(compat)),
        f("grid", Attr::List(vec![Attr::Int(grid.0 as i64), Attr::Int(grid.1 as i64)])),
        f("cloud_buffer_m", Attr::Float(300.0)),
    ]);
    v
}

/// The atmospheric correction of a Sentinel-2 Level 1C scene to surface reflectance (`level2` of `zeit/_level2.py`): the
/// inputs of [`crate::level2`], the Fmask unless `opts.fmask` is false, the merge of `opts.merge` on the TOA between the
/// flags and the correction, and zeit-core's `level2_scene` with the sun and the BRDF factor of the core.
pub fn level2(safe: &Safe, opts: &Level2Options) -> Result<Level2Output, String> {
    if opts.topography && opts.dem.is_none() {
        return Err("level2: topography=true needs the DEM: pass a dem (a raster in metres) or switch topography off".into());
    }
    let n_jobs = opts.n_jobs.clamp(1, 8);
    let desc = safe.describe()?;
    let (nr, nc) = (desc.nrows, desc.ncols);
    let n = nr * nc;
    let dn = level2::read_bands(safe, &desc, n_jobs)?;
    let dem_array: Option<Vec<f32>> = match &opts.dem {
        Some(d) => Some(level2::dem_on_grid_with(d, safe, None, Some(&desc), &gdal_method(&opts.dem_resampling), n_jobs)?),
        None => None,
    };
    let mut fmask = None;
    let flags: Option<Vec<i16>> = if opts.fmask {
        let (f, info) = level2::fmask_flags(
            safe,
            &desc,
            &dn,
            opts.dem.as_deref().map(FmaskDem::File),
            opts.gswo.as_deref(),
            opts.fmask_compat,
            n_jobs,
        )?;
        fmask = Some(info);
        Some(f)
    } else {
        None
    };
    let (lon, lat) = level2::geo_tables(safe, &desc)?;
    let g = desc.geo_grid;
    if lon.len() != g.nx * g.ny || lat.len() != g.nx * g.ny {
        return Err(format!("the geography needs {} x {} corners", g.nx, g.ny));
    }
    let table = GeoTable { ulx: g.ulx, uly: g.uly, res: g.res, nx: g.nx, ny: g.ny, lon, lat };
    let sp = SceneParams {
        l2: L2Params {
            domulti: opts.multiple_scattering,
            doenv: opts.adjacency,
            doatmo: true,
            dotopo: opts.topography,
            dobrdf: opts.brdf,
            erase_cloud: opts.erase_cloud,
            doaod: opts.aod,
            odst: true,
            ovzn: true,
            ohot: true,
            oaod: true,
            owvp: true,
        },
        dem_nodata: DEM_NODATA,
        buffer_nodata: false,
        n_jobs: n_jobs as i32,
    };
    let bands: Vec<&[u16]> = dn.chunks(n.max(1)).collect();
    let merge = opts.merge;
    let mut hook = |toa: &mut Toa, q: &mut [i16]| -> Result<(), L2Error> {
        if let Some(m) = merge {
            toa_merge(toa, q, m, n_jobs as i32)?;
        }
        Ok(())
    };
    let t0 = Instant::now();
    let out = l2scene::level2_scene(
        &desc.l1,
        &bands,
        flags.as_deref(),
        dem_array.as_deref(),
        &table,
        None,
        None,
        &sp,
        if merge.is_some() { Some(&mut hook) } else { None },
    )
    .map_err(|e| e.to_string())?;
    let core_seconds = t0.elapsed().as_secs_f64();
    drop(bands);
    drop((dn, flags, dem_array, table));
    let (l2, atc) = (out.level2, out.atc);

    if l2.boa.bands.len() != BOA_BANDS.len() {
        return Err(format!("the correction gave {} bands, expected {}", l2.boa.bands.len(), BOA_BANDS.len()));
    }
    let boa = l2.boa.bands.concat();
    let (epsg, date, p) = (desc.l1.tile.epsg.ok_or("the metadata has no EPSG code")?, desc.l1.product.date, &desc.l1.product);

    // the water vapour of the scene: the mean of the pixels with data (int16 sums are exact, as numpy's float64 ones)
    let (sum, count) = l2.wvp.as_deref().unwrap_or(&[]).iter().filter(|&&v| v != BOA_NODATA).fold((0i64, 0usize), |(s, c), &v| (s + i64::from(v), c + 1));
    let attrs = Level2Attrs {
        datetime: format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", date.year, date.month, date.day, date.hh, date.mm, date.ss),
        sensor: p.sensor.clone(),
        baseline: p.baseline.clone().unwrap_or_else(|| "None".into()),
        aod_green: atc.aod.get(2).map_or(f64::NAN, |&v| f64::from(v)),
        aod_map: atc.aodmap,
        water_vapor_cm: if count > 0 { (sum as f64 / count as f64) / 1000.0 } else { f64::NAN },
        n_targets: l2.n_targets,
        core_seconds,
        merge: merge.map(Merge::name),
        fmask: fmask
            .as_ref()
            .map(|i| fmask_attrs(i, &p.baseline, (nr / 2, nc / 2), opts.dem.is_some(), opts.gswo.is_some(), opts.fmask_compat))
            .unwrap_or_default(),
        provenance: PROVENANCE,
    };
    debug_assert_eq!(NB, 13);
    Ok(Level2Output {
        nrows: nr,
        ncols: nc,
        epsg,
        ulx: desc.l1.tile.ulx,
        uly: desc.l1.tile.uly,
        res: desc.l1.tile.res,
        boa,
        qai: l2.qai,
        dst: l2.dst,
        vzn: l2.vzn,
        hot: l2.hot,
        aod: l2.aod,
        wvp: l2.wvp,
        attrs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeit_core::gas::SceneGrid;

    fn toa_of(n_side: usize, f: impl Fn(usize, usize) -> i16) -> Toa {
        let n = n_side * n_side;
        let domain: Vec<String> = zeit_core::s2l1c::DOMAINS.iter().map(|d| d.to_string()).collect();
        let bands = (0..NB).map(|b| (0..n).map(|k| f(b, k)).collect()).collect();
        Toa { grid: SceneGrid { nx: n_side, ny: n_side, res: 10.0 }, bands, domain }
    }

    #[test]
    fn merge_leaves_off_and_the_ten_metre_bands_alone() {
        let side = 12;
        let n = side * side;
        let mut toa = toa_of(side, |b, k| 500 + (b as i16) * 30 + ((k * 7) % 40) as i16);
        let mut qai = vec![0i16; n];
        qai[5] = 1; // off
        for b in toa.bands.iter_mut() {
            b[5] = TOA_NODATA;
        }
        let before = toa.bands.clone();
        for m in [Merge::Regression, Merge::Improphe] {
            let mut t = toa.clone();
            toa_merge(&mut t, &qai, m, 2).unwrap();
            let predicted: Vec<usize> = MERGE_DOMAINS[3..].iter().map(|d| t.find_domain(d).unwrap()).collect();
            for b in 0..NB {
                if predicted.contains(&b) {
                    assert_eq!(t.bands[b][5], TOA_NODATA, "{m:?} band {b}: the off pixel stays off");
                } else {
                    assert_eq!(t.bands[b], before[b], "{m:?} band {b} is not a merged one");
                }
            }
        }
        // a QAI of the wrong size is an error, not a panic
        assert!(toa_merge(&mut toa, &qai[1..], Merge::Improphe, 1).is_err());
    }

    #[test]
    fn merge_names_and_options() {
        assert_eq!(Merge::parse(None).unwrap(), None);
        assert_eq!(Merge::parse(Some("improphe")).unwrap(), Some(Merge::Improphe));
        assert!(Merge::parse(Some("x")).unwrap_err().contains("regression"));
        let o = Level2Options::default();
        assert!(o.brdf && o.topography && o.aod && o.adjacency && o.multiple_scattering && o.fmask);
        assert!(!o.erase_cloud && !o.fmask_compat && o.merge.is_none() && o.n_jobs == 8);
    }

    #[test]
    fn topography_without_a_dem_is_an_error() {
        let e = level2(&Safe { path: "x".into(), product: "x".into(), granule: "x".into(), tile: "x".into(), bands: vec![] }, &Level2Options::default())
            .unwrap_err();
        assert!(e.contains("needs the DEM"), "{e}");
    }

    #[test]
    fn attrs_keep_zeits_order() {
        let a = Level2Attrs {
            datetime: "2023-07-28T13:22:39Z".into(),
            sensor: "SEN2B".into(),
            baseline: "05.09".into(),
            aod_green: 0.1,
            aod_map: true,
            water_vapor_cm: f64::NAN,
            n_targets: [1, 2, 3],
            core_seconds: 1.0,
            merge: Some("improphe"),
            fmask: vec![("cloud".into(), Attr::Float(0.5))],
            provenance: PROVENANCE,
        };
        let keys: Vec<String> = a.pairs().into_iter().map(|(k, _)| k).collect();
        assert_eq!(
            keys,
            ["datetime", "sensor", "baseline", "aod_green", "aod_map", "water_vapor_cm", "n_targets", "core_seconds", "merge", "fmask_cloud", "provenance"]
        );
    }
}
