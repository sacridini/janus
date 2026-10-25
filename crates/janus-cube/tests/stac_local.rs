//! A STAC series built from items whose assets are local files (no network): the mosaic of a day's items, a band
//! of coarser cells, an item in the next UTM zone warped onto the grid, an item without proj: metadata (its grid
//! read from its file), opened as a cube with its dates, wavelengths, NDVI and the SCL mask.

use std::sync::atomic::AtomicBool;

use gdal::DriverManager;
use gdal::raster::Buffer;
use gdal::spatial_ref::SpatialRef;
use janus_cube::stac::{self, parse_item};
use janus_cube::stac_vrt::{BuildSpec, build};
use janus_cube::{BandSelection, QaRule, open_cube, with_thread_reader};
use serde_json::json;

/// A GeoTIFF of one band filled with `value` (u16), its grid given.
fn tif(path: &std::path::Path, epsg: u32, gt: [f64; 6], w: usize, h: usize, value: u16) {
    let d = DriverManager::get_driver_by_name("GTiff").unwrap();
    let mut ds = d.create_with_band_type::<u16, _>(path, w, h, 1).unwrap();
    ds.set_geo_transform(&gt).unwrap();
    ds.set_spatial_ref(&SpatialRef::from_epsg(epsg).unwrap()).unwrap();
    let mut b = ds.rasterband(1).unwrap();
    b.set_no_data_value(Some(0.0)).unwrap();
    let mut buf = Buffer::new((w, h), vec![value; w * h]);
    b.write((0, 0), (w, h), &mut buf).unwrap();
}

fn asset(
    path: &std::path::Path,
    common: Option<(&str, f64)>,
    gt: Option<[f64; 6]>,
    shape: [usize; 2],
) -> serde_json::Value {
    let mut a = json!({
        "href": path.to_string_lossy(),
        "type": "image/tiff; application=geotiff; profile=cloud-optimized",
        "roles": ["data"],
        "raster:bands": [{"nodata": 0, "data_type": "uint16"}],
    });
    if let Some((c, um)) = common {
        a["eo:bands"] = json!([{"common_name": c, "center_wavelength": um}]);
        a["raster:bands"][0]["scale"] = json!(0.0001);
    }
    if let Some(g) = gt {
        // STAC's order: a, b, c, d, e, f
        a["proj:transform"] = json!([g[1], g[2], g[0], g[4], g[5], g[3]]);
        a["proj:shape"] = json!(shape);
    }
    a
}

#[test]
fn a_local_series() {
    janus_cube::remote::configure_remote_access();
    let dir = std::env::temp_dir().join("janus_stac_local");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("files")).unwrap();
    let f = |n: &str| dir.join("files").join(n);
    // zone 20 S: a 10 m tile (red 1000, NIR 3000 on day 1; 1500, 4500 on day 2), its SCL at 20 m (4 = vegetation;
    // 9 = cloud on the right half of day 2); zone 21 S: a tile east of 60 W (red 2000, NIR 2000)
    let gt20 = [826000.0, 10.0, 0.0, 8899000.0, 0.0, -10.0];
    let gt20s = [826000.0, 20.0, 0.0, 8899000.0, 0.0, -20.0];
    let gt21 = [171000.0, 10.0, 0.0, 8899000.0, 0.0, -10.0];
    for (day, red, nir) in [(1, 1000, 3000), (2, 1500, 4500)] {
        tif(&f(&format!("a{day}_red.tif")), 32720, gt20, 400, 300, red);
        tif(&f(&format!("a{day}_nir.tif")), 32720, gt20, 400, 300, nir);
        tif(&f(&format!("a{day}_scl.tif")), 32720, gt20s, 200, 150, 4);
    }
    // the cloud: day 2's SCL is 9 on its right half
    {
        let ds = gdal::Dataset::open_ex(
            f("a2_scl.tif"),
            gdal::DatasetOptions { open_flags: gdal::GdalOpenFlags::GDAL_OF_UPDATE, ..Default::default() },
        )
        .unwrap();
        let mut b = ds.rasterband(1).unwrap();
        let mut buf = Buffer::new((100, 150), vec![9u16; 100 * 150]);
        b.write((100, 0), (100, 150), &mut buf).unwrap();
    }
    tif(&f("b1_red.tif"), 32721, gt21, 400, 300, 2000);
    tif(&f("b1_nir.tif"), 32721, gt21, 400, 300, 2000);
    tif(&f("b1_scl.tif"), 32721, [171000.0, 20.0, 0.0, 8899000.0, 0.0, -20.0], 200, 150, 4);
    let item = |id: &str, date: &str, epsg: u32, pre: &str, proj: bool, gt: [f64; 6], gts: [f64; 6]| {
        let g = |x: [f64; 6]| proj.then_some(x);
        let mut props = json!({"datetime": format!("{date}T14:00:00Z"), "eo:cloud_cover": 1.0, "grid:code": pre});
        if proj {
            props["proj:epsg"] = json!(epsg);
        }
        let v = json!({
            "type": "Feature", "id": id, "collection": "test",
            "geometry": {"type": "Polygon", "coordinates": [[[-60.05, -10.0], [-59.95, -10.0], [-59.95, -9.9], [-60.05, -9.9], [-60.05, -10.0]]]},
            "properties": props,
            "assets": {
                "red": asset(&f(&format!("{pre}_red.tif")), Some(("red", 0.665)), g(gt), [300, 400]),
                "nir": asset(&f(&format!("{pre}_nir.tif")), Some(("nir", 0.842)), g(gt), [300, 400]),
                "scl": asset(&f(&format!("{pre}_scl.tif")), None, g(gts), [150, 200]),
            },
        });
        parse_item(&v).unwrap()
    };
    let items = vec![
        item("a1", "2024-07-01", 32720, "a1", true, gt20, gt20s),
        // day 1 also has the tile of the next zone, without proj: metadata
        item("b1", "2024-07-01", 32721, "b1", false, gt21, gt21),
        item("a2", "2024-07-06", 32720, "a2", true, gt20, gt20s),
    ];
    let choices = stac::band_choices(&items);
    assert_eq!(choices.iter().map(|c| c.key.as_str()).collect::<Vec<_>>(), ["red", "nir", "scl"]);
    // the area: across 60 W, from the zone 20 tile into the zone 21 one
    let bbox = [-60.02, -9.9640, -59.99, -9.9555];
    let spec = BuildSpec { items, bands: choices, bbox, res: None, dir: dir.join("series"), planetary: false };
    let built = build(&spec, &|_, _| {}, &AtomicBool::new(false)).expect("build");
    assert_eq!(built.files.len(), 2);
    assert_eq!(built.grid.crs, "EPSG:32720");
    assert_eq!(built.grid.gt[1], 10.0);
    assert_eq!(built.warped, 3, "b1's three bands warped");
    let info = open_cube(&built.files, &BandSelection::default()).expect("open");
    assert!(info.time_is_date && info.t() == 2);
    assert_eq!(info.band_names[2], "SCL (scene classification)");
    assert_eq!(info.band_nm, [665.0, 842.0, 0.0]);
    assert_eq!((info.sel.band, info.sel.nd_band, info.sel.qa_band, info.sel.qa_rule), (2, 1, 3, QaRule::Scl));
    let info = std::sync::Arc::new(info);
    let (w, h) = (info.width, info.height);
    let mut v = vec![0f32; w * h];
    // day 1: NDVI 0.5 over zone 20 (west), 0 over zone 21 (east, warped)
    assert!(with_thread_reader(&info, |r| r.read_window(0, (0, 0, w, h), &mut v, (w, h))));
    let ndvi = |a: f64, b: f64| ((b - a) / (b + a)) as f32;
    let (west, east) = (v[h / 2 * w + 2], v[h / 2 * w + w - 3]);
    assert!((west - ndvi(0.1, 0.3)).abs() < 1e-6, "west {west}");
    assert!(east.abs() < 1e-6, "east {east}");
    // day 2: the zone 20 tile alone; the cloud masked where the SCL says 9
    assert!(with_thread_reader(&info, |r| r.read_window(1, (0, 0, w, h), &mut v, (w, h))));
    let finite = v.iter().filter(|x| x.is_finite()).count();
    assert!(finite > 0 && finite < w * h, "some masked, some not ({finite} of {})", w * h);
    assert!(v.iter().filter(|x| x.is_finite()).all(|x| (x - ndvi(0.15, 0.45)).abs() < 1e-6));
}
