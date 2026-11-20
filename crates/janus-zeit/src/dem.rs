//! An elevation model on the grid of a run (the topographic correction): a file of the user's, or the Copernicus DEM
//! GLO-30 or GLO-90 read from Planetary Computer (Earth Search when it does not answer), mosaicked and warped
//! onto the grid by GDAL's own warp (`-et 0`, as `level2::warp_to_grid`), kept as a COG under the results folder for
//! the next run, with the licence's notice in its metadata.
//!
//! Copernicus DEM: "Licence for the use of the Copernicus WorldDEM-30" (free, worldwide, no time limit; reproduction,
//! adaptation and combination allowed) asks for the notice below on what is adapted from it, and that the organisations
//! of the Copernicus programme are not held liable.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use gdal::raster::{Buffer, RasterCreationOptions};
use gdal::spatial_ref::{AxisMappingStrategy, CoordTransform, SpatialRef};
use gdal::{Dataset, DriverManager, Metadata};
use janus_cube::stac::{self, Search};

use crate::warp::Grid;

/// The no-data value of a DEM on a grid (FORCE's, as `level2::DEM_NODATA`).
pub const NODATA: f32 = -32767.0;

pub const GLO30: &str = "cop-dem-glo-30";
pub const GLO90: &str = "cop-dem-glo-90";

const PLANETARY: &str = "https://planetarycomputer.microsoft.com/api/stac/v1";
const EARTH_SEARCH: &str = "https://earth-search.aws.element84.com/v1";

/// The notice the licence asks for on what is adapted from the DEM (the warped model, what is made with it).
pub fn notice(collection: &str) -> String {
    let product = if collection == GLO90 { "WorldDEM-90" } else { "WorldDEM-30" };
    format!(
        "produced using Copernicus {product} \u{a9} DLR e.V. 2010-2014 and \u{a9} Airbus Defence and Space GmbH \
         2014-2018 provided under COPERNICUS by the European Union and ESA; all rights reserved"
    )
}

/// The sentence the licence asks for on redistribution.
pub fn liability(collection: &str) -> String {
    let product = if collection == GLO90 { "WorldDEM-90" } else { "WorldDEM-30" };
    format!(
        "The organisations in charge of the Copernicus programme by law or by delegation do not incur any liability \
         for any use of the Copernicus {product}."
    )
}

/// A short hash of a grid (its CRS, geotransform and size), for the cache's file name (FNV-1a, 64 bits).
pub fn grid_hash(grid: &Grid) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    let mut feed = |bytes: &[u8]| {
        for b in bytes {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x100000001b3);
        }
    };
    feed(grid.wkt.as_bytes());
    for v in grid.gt {
        feed(&v.to_bits().to_le_bytes());
    }
    feed(&(grid.w as u64).to_le_bytes());
    feed(&(grid.h as u64).to_le_bytes());
    format!("{h:016x}")
}

/// The DEM of a file on `grid` (`f32`, NaN where it has no value): GDAL's warp with `method` (`bilinear`, `cubic`,
/// `near`), the source's no-data value, or -32767 where it has none.
pub fn on_grid(path: &Path, grid: &Grid, method: &str, n_jobs: usize) -> Result<Vec<f32>, String> {
    let mut v = crate::level2::warp_to_grid::<f32>(path, grid, method, f64::from(NODATA), f64::from(NODATA), n_jobs)?;
    for x in &mut v {
        if *x == NODATA || !x.is_finite() {
            *x = f32::NAN;
        }
    }
    Ok(v)
}

/// The grid's extent as longitude and latitude (west, south, east, north), with a margin of two pixels on each side and
/// 0.01 degree.
pub fn lon_lat_box(grid: &Grid) -> Result<[f64; 4], String> {
    let mut from = SpatialRef::from_wkt(&grid.wkt).map_err(|e| format!("the grid's CRS: {e}"))?;
    from.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    let mut to = SpatialRef::from_epsg(4326).map_err(|e| e.to_string())?;
    to.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    let t = CoordTransform::new(&from, &to).map_err(|e| e.to_string())?;
    let g = &grid.gt;
    let (margin_x, margin_y) = (2.0, 2.0);
    // the border, so that a curved edge is covered
    let (mut xs, mut ys) = (vec![], vec![]);
    let steps = 8;
    for i in 0..=steps {
        for j in 0..=steps {
            if i != 0 && i != steps && j != 0 && j != steps {
                continue;
            }
            let c = -margin_x + (grid.w as f64 + 2.0 * margin_x) * f64::from(i) / f64::from(steps);
            let r = -margin_y + (grid.h as f64 + 2.0 * margin_y) * f64::from(j) / f64::from(steps);
            xs.push(g[0] + c * g[1] + r * g[2]);
            ys.push(g[3] + c * g[4] + r * g[5]);
        }
    }
    let mut zs = vec![0.0; xs.len()];
    t.transform_coords(&mut xs, &mut ys, &mut zs).map_err(|e| format!("the grid to longitude and latitude: {e}"))?;
    let fold = |v: &[f64], f: fn(f64, f64) -> f64, init: f64| v.iter().copied().filter(|x| x.is_finite()).fold(init, f);
    let b = [
        fold(&xs, f64::min, f64::INFINITY) - 0.01,
        fold(&ys, f64::min, f64::INFINITY) - 0.01,
        fold(&xs, f64::max, f64::NEG_INFINITY) + 0.01,
        fold(&ys, f64::max, f64::NEG_INFINITY) + 0.01,
    ];
    if b.iter().any(|v| !v.is_finite()) {
        return Err("the grid has no extent in longitude and latitude".into());
    }
    Ok(b)
}

/// The data assets' paths (readable by GDAL) of the items of a collection, the containers of Planetary Computer's
/// signed first.
pub fn tile_paths(items: &[stac::Item], planetary: bool) -> Result<Vec<String>, String> {
    let mut out = vec![];
    for it in items {
        let Some(a) = it.assets.get("data") else { continue };
        if planetary && let Some((account, container, _)) = stac::azure_blob(&a.href) {
            stac::sign_planetary(&it.collection, &account, &container)?;
        }
        out.push(stac::asset_path(&a.href, planetary));
    }
    if out.is_empty() {
        return Err("the catalog gave no elevation tile for this area".into());
    }
    Ok(out)
}

/// Writes `data` (the DEM on `grid`) as a COG with the licence's notice in its metadata.
pub fn write_cog(path: &Path, grid: &Grid, data: &[f32], notice: Option<&str>) -> Result<(), String> {
    let mem = DriverManager::get_driver_by_name("MEM").map_err(|e| e.to_string())?;
    let mut ds = mem.create_with_band_type::<f32, _>("", grid.w, grid.h, 1).map_err(|e| e.to_string())?;
    ds.set_geo_transform(&grid.gt).map_err(|e| e.to_string())?;
    ds.set_projection(&grid.wkt).map_err(|e| e.to_string())?;
    {
        let mut band = ds.rasterband(1).map_err(|e| e.to_string())?;
        band.set_no_data_value(Some(f64::from(NODATA))).map_err(|e| e.to_string())?;
        let values: Vec<f32> = data.iter().map(|&v| if v.is_finite() { v } else { NODATA }).collect();
        band.write((0, 0), (grid.w, grid.h), &mut Buffer::new((grid.w, grid.h), values)).map_err(|e| e.to_string())?;
        band.set_description("elevation (m)").map_err(|e| e.to_string())?;
    }
    if let Some(n) = notice {
        ds.set_metadata_item("COPERNICUS_DEM_NOTICE", n, "").map_err(|e| e.to_string())?;
        ds.set_metadata_item("TIFFTAG_COPYRIGHT", n, "").map_err(|e| e.to_string())?;
    }
    let driver = DriverManager::get_driver_by_name("COG").map_err(|e| e.to_string())?;
    let options = RasterCreationOptions::from_iter(["COMPRESS=DEFLATE", "PREDICTOR=3", "BLOCKSIZE=256"]);
    ds.create_copy(&driver, path, &options).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(())
}

/// The mosaic of `sources` warped onto `grid` (`f32`, [`NODATA`] where there is none): a VRT of them in `scratch`,
/// gone when it is done.
pub fn mosaic_on_grid(
    sources: &[String],
    grid: &Grid,
    method: &str,
    n_jobs: usize,
    scratch: &Path,
) -> Result<Vec<f32>, String> {
    let vrt = scratch.join(format!("dem_mosaic_{}.vrt", grid_hash(grid)));
    crate::series_io::build_vrt(&vrt, sources, &["-resolution".into(), "highest".into()])?;
    let r = on_grid(&vrt, grid, method, n_jobs);
    let _ = std::fs::remove_file(&vrt);
    r
}

/// The Copernicus DEM (`collection` GLO-30 or GLO-90) on `grid`, as a COG under `cache_dir`: the file of an earlier run
/// for the same grid is reused. The tiles over the grid's extent are searched on Planetary Computer, then (if it does
/// not answer) on Earth Search.
pub fn copernicus(
    collection: &str,
    grid: &Grid,
    cache_dir: &Path,
    method: &str,
    n_jobs: usize,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<PathBuf, String> {
    if ![GLO30, GLO90].contains(&collection) {
        return Err(format!("{collection} is not a Copernicus DEM collection (cop-dem-glo-30 or cop-dem-glo-90)"));
    }
    std::fs::create_dir_all(cache_dir).map_err(|e| format!("{}: {e}", cache_dir.display()))?;
    let file = cache_dir.join(format!("{collection}_{}_{method}.tif", grid_hash(grid)));
    if file.is_file() && Dataset::open(&file).is_ok() {
        progress(0.0, "the elevation model of an earlier run");
        return Ok(file);
    }
    let bbox = lon_lat_box(grid)?;
    let mut failures: Vec<String> = vec![];
    for (name, url, planetary) in [("Planetary Computer", PLANETARY, true), ("Earth Search", EARTH_SEARCH, false)] {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        progress(0.0, &format!("looking for the {collection} tiles on {name}"));
        let search = Search {
            url: url.into(),
            collection: collection.into(),
            bbox,
            start: "2021-04-01".into(),
            end: "2021-05-01".into(),
            max_cloud: None,
            max_items: 500,
        };
        let attempt = (|| -> Result<Vec<f32>, String> {
            let items = stac::search(&search, &|_| {}, cancel)?;
            let sources = tile_paths(&items, planetary)?;
            progress(0.0, &format!("reading {} elevation tiles from {name}", sources.len()));
            mosaic_on_grid(&sources, grid, method, n_jobs, cache_dir)
        })();
        match attempt {
            Ok(data) => {
                if data.iter().all(|v| v.is_nan()) {
                    failures.push(format!("{name}: the tiles have no elevation over this area"));
                    continue;
                }
                let tmp = file.with_extension("partial.tif");
                write_cog(&tmp, grid, &data, Some(&notice(collection)))?;
                std::fs::rename(&tmp, &file).map_err(|e| format!("{}: {e}", file.display()))?;
                return Ok(file);
            }
            Err(e) if e == "cancelled" => return Err(e),
            Err(e) => failures.push(format!("{name}: {e}")),
        }
    }
    Err(format!("the Copernicus DEM could not be read ({}): check the network or choose 'A file'", failures.join("; ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A GeoTIFF in EPSG:4326 of `w` x `h` cells of `step` degrees from (`x0`, `y0`) with `f(lon, lat)` metres.
    fn tile(path: &Path, x0: f64, y0: f64, step: f64, w: usize, h: usize, f: impl Fn(f64, f64) -> f32) {
        let mut ds =
            DriverManager::get_driver_by_name("GTiff").unwrap().create_with_band_type::<f32, _>(path, w, h, 1).unwrap();
        ds.set_geo_transform(&[x0, step, 0.0, y0, 0.0, -step]).unwrap();
        ds.set_projection(&SpatialRef::from_epsg(4326).unwrap().to_wkt().unwrap()).unwrap();
        let v: Vec<f32> =
            (0..w * h).map(|i| f(x0 + ((i % w) as f64 + 0.5) * step, y0 - ((i / w) as f64 + 0.5) * step)).collect();
        let mut b = ds.rasterband(1).unwrap();
        b.set_no_data_value(Some(-32767.0)).unwrap();
        b.write((0, 0), (w, h), &mut Buffer::new((w, h), v)).unwrap();
    }

    fn utm_grid() -> Grid {
        // 20 x 20 cells of 30 m in UTM 23S, near -46.5 / -22.5
        let wkt = SpatialRef::from_epsg(32723).unwrap().to_wkt().unwrap();
        Grid { wkt, gt: [350000.0, 30.0, 0.0, 7510000.0, 0.0, -30.0], w: 20, h: 20 }
    }

    /// A plane in latitude warped onto a UTM grid stays a plane (bilinear), and -32767 is NaN; the file is cached.
    #[test]
    fn a_dem_in_degrees_is_warped_onto_a_utm_grid() {
        let dir = std::env::temp_dir().join("janus-zeit-test-dem-warp");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let grid = utm_grid();
        // two tiles side by side (the mosaic), the elevation rising with the latitude by 100 m a degree
        let (a, b) = (dir.join("a.tif"), dir.join("b.tif"));
        let f = |_lon: f64, lat: f64| (1000.0 + 100.0 * (lat + 23.0)) as f32;
        tile(&a, -47.0, -22.0, 0.001, 500, 1000, f);
        tile(&b, -46.5, -22.0, 0.001, 500, 1000, f);
        let sources = vec![a.to_string_lossy().into_owned(), b.to_string_lossy().into_owned()];
        let dem = mosaic_on_grid(&sources, &grid, "bilinear", 1, &dir).unwrap();
        assert_eq!(dem.len(), 400);
        assert!(dem.iter().all(|v| v.is_finite()), "the mosaic covers the grid");
        // north is up: the first row is the highest, by 30 m of 100 m per 111 km
        let (north, south) = (dem[10], dem[10 + 19 * 20]);
        assert!(north > south && (north - south - 19.0 * 30.0 / 111_000.0 * 100.0).abs() < 0.02, "{north} {south}");
        // a hole of nodata is NaN
        let hole = dir.join("hole.tif");
        tile(&hole, -47.0, -22.0, 0.001, 1000, 1000, |_, _| -32767.0);
        let dem = on_grid(&hole, &grid, "bilinear", 1).unwrap();
        assert!(dem.iter().all(|v| v.is_nan()));
        // the COG: the notice in its metadata, read back on the same grid
        let cog = dir.join("dem.tif");
        let data = mosaic_on_grid(&sources, &grid, "bilinear", 1, &dir).unwrap();
        write_cog(&cog, &grid, &data, Some(&notice(GLO30))).unwrap();
        let ds = Dataset::open(&cog).unwrap();
        assert!(
            ds.metadata_item("COPERNICUS_DEM_NOTICE", "").unwrap().starts_with("produced using Copernicus WorldDEM-30")
        );
        assert_eq!(on_grid(&cog, &grid, "bilinear", 1).unwrap(), data);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_notice_names_the_product_and_the_cache_names_the_grid() {
        assert!(notice(GLO90).contains("WorldDEM-90") && notice(GLO30).contains("WorldDEM-30"));
        assert!(notice(GLO30).contains("provided under COPERNICUS by the European Union and ESA; all rights reserved"));
        let g = utm_grid();
        let moved = Grid { gt: [350030.0, 30.0, 0.0, 7510000.0, 0.0, -30.0], ..g.clone() };
        assert_eq!(grid_hash(&g), grid_hash(&g.clone()));
        assert_ne!(grid_hash(&g), grid_hash(&moved));
        let b = lon_lat_box(&g).unwrap();
        assert!(b[0] < -46.0 && b[2] > -47.5 && b[1] < -22.0 && b[3] > -23.0 && b[0] < b[2] && b[1] < b[3], "{b:?}");
    }

    /// The catalog's answer: its items' `data` assets are the tiles; an item without one is left out; an answer with
    /// no tile is an error.
    #[test]
    fn the_tiles_are_the_items_data_assets() {
        let feature = |id: &str, href: &str| {
            serde_json::json!({
                "id": id, "collection": GLO30,
                "geometry": null,
                "properties": { "datetime": "2021-04-22T00:00:00Z" },
                "assets": { "data": { "href": href, "type": "image/tiff; application=geotiff; profile=cloud-optimized" } },
            })
        };
        let items: Vec<stac::Item> =
            [feature("a", "/tiles/a.tif"), feature("b", "/tiles/b.tif")].iter().filter_map(stac::parse_item).collect();
        assert_eq!(items.len(), 2);
        let paths = tile_paths(&items, false).unwrap();
        assert_eq!(paths.len(), 2);
        assert!(paths[0].ends_with("a.tif"));
        assert!(tile_paths(&[], false).is_err());
    }

    /// A cached DEM of the same grid is used without a search.
    #[test]
    fn a_cached_dem_is_reused() {
        let dir = std::env::temp_dir().join("janus-zeit-test-dem-cache");
        let _ = std::fs::remove_dir_all(&dir);
        let grid = utm_grid();
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(format!("{GLO30}_{}_bilinear.tif", grid_hash(&grid)));
        write_cog(&file, &grid, &vec![1500.0; 400], Some(&notice(GLO30))).unwrap();
        let got = copernicus(GLO30, &grid, &dir, "bilinear", 1, &|_, _| {}, &AtomicBool::new(false)).unwrap();
        assert_eq!(got, file);
        assert!(copernicus("cop-dem-glo-10", &grid, &dir, "bilinear", 1, &|_, _| {}, &AtomicBool::new(false)).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
