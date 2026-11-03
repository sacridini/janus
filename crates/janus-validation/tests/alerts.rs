//! Official alerts against a map of change, end to end on data made here: DETER-like polygons served by the test
//! OGC server (janus_geo::ogc_mock: 10 x 6 squares of 600 m, one a day from 2020-01-01, every third a
//! degradation) fetched by WFS into a GeoPackage, GLAD-S2 and RADD rasters written here, and a map of change
//! dates over part of the grid; every number checked exactly.

use std::sync::atomic::AtomicBool;

use gdal::spatial_ref::SpatialRef;
use janus_geo::ogc::{self, FeatureRequest, Service};
use janus_geo::ogc_mock::{MockData, MockServer};
use janus_validation::Grid;
use janus_validation::alerts::{self, CompareOptions, Product, RasterFile, days_from_civil, decimal_year};

fn dir() -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("janus_alerts_test_{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The series' grid: 300 x 200 pixels of 30 m from (500000, 7500000) in UTM 23S, under the mock's squares (each
/// 20 x 20 pixels, 5 pixels in from its cell of 30).
fn grid(cell: f64, w: usize, h: usize) -> Grid {
    Grid {
        w,
        h,
        gt: [500000.0, cell, 0.0, 7500000.0, 0.0, -cell],
        has_gt: true,
        wkt: SpatialRef::from_epsg(32723).unwrap().to_wkt().unwrap(),
    }
}

/// The map: changed on 2020-03-01 in columns 0-149 and rows 0-119 (the squares of columns 0-4, rows 0-3).
fn change_map(g: &Grid) -> Vec<f32> {
    let d = decimal_year(days_from_civil(2020, 3, 1)) as f32;
    (0..g.w * g.h).map(|i| if i % g.w < 150 && i / g.w < 120 { d } else { f32::NAN }).collect()
}

#[test]
fn deter_polygons_from_wfs() {
    gdal::DriverManager::register_all();
    let server = MockServer::start(MockData::default()).unwrap();
    let d = dir();
    let out = d.join("deter.gpkg");
    let req = FeatureRequest {
        service: Service::Wfs,
        url: server.url("/ows"),
        layer: "test:plots".into(),
        ..Default::default()
    };
    let f = ogc::fetch_features(&req, &out, &|_, _| {}, &AtomicBool::new(false)).unwrap();
    assert_eq!(f.features, 60);
    let g = grid(30.0, 300, 200);
    // the map written and read back as Janus reads a result
    let map_path = d.join("change.tif").to_string_lossy().into_owned();
    alerts::write_f32(&map_path, &g, &change_map(&g)).unwrap();
    let (mg, change) = alerts::read_change_map(&map_path, 1).unwrap();
    assert_eq!((mg.w, mg.h), (300, 200));
    let a = alerts::alerts_from_vector(&out.to_string_lossy(), &f.layer, "day", "classname", &mg).unwrap();
    assert_eq!(a.units.len(), 60);
    assert_eq!(a.classes, ["DEGRADACAO", "DESMATAMENTO_CR"]);
    assert!((a.units[0].area_ha - 36.0).abs() < 1e-9);
    let year = CompareOptions { from: days_from_civil(2020, 1, 1), to: days_from_civil(2020, 12, 31), bin_days: 30 };
    let c = alerts::compare(&change, &a, &year);
    // 20 squares of 400 pixels in both; the map's 18000 pixels; the alerts' 24000
    assert_eq!((c.both, c.only_map, c.only_alerts), (8000, 10000, 16000));
    assert!((c.both_ha - 720.0).abs() < 1e-6 && (c.only_map_ha - 900.0).abs() < 1e-6);
    assert_eq!((c.alerts, c.alerts_detected, c.alerts_too_small), (60, 20, 0));
    assert_eq!((c.map_patches, c.map_patches_confirmed), (1, 1));
    assert!((c.alerts_detected_share() - 1.0 / 3.0).abs() < 1e-12);
    assert!((c.map_confirmed_share() - 4.0 / 9.0).abs() < 1e-12);
    // lags: 2020-03-01 minus the squares' days (61 - id for ids 1-5, 11-15, 21-25, 31-35), 400 pixels each
    assert_eq!(c.lag.n, 8000);
    assert_eq!((c.lag.min, c.lag.median, c.lag.max), (26.0, 43.0, 60.0));
    assert!((c.lag.mean - 43.0).abs() < 1e-9);
    assert_eq!(c.histogram, vec![(0, 30, 1600), (30, 60, 6000), (60, 90, 400)]);
    // by class: the degradations detected are squares 3, 12, 15, 21, 24 and 33
    assert_eq!((c.classes[0].alerts, c.classes[0].detected), (20, 6));
    assert_eq!((c.classes[1].alerts, c.classes[1].detected), (40, 14));
    let u1 = c.units.iter().find(|u| u.unit.id == 1).unwrap();
    assert_eq!((u1.pixels, u1.detected_pixels, u1.lag_median), (400, 400, 60.0));
    // January only: the map's change is later
    let jan = CompareOptions { from: days_from_civil(2020, 1, 1), to: days_from_civil(2020, 1, 31), bin_days: 30 };
    let c = alerts::compare(&change, &a, &jan);
    assert_eq!((c.both, c.only_map, c.only_alerts), (0, 0, 31 * 400));
    assert_eq!((c.alerts, c.alerts_detected, c.lag.n), (31, 0, 0));
    // the exports
    let files = c.export(&d.join("out"), "deter", &map_path, &a).unwrap();
    assert_eq!(files.len(), 5);
    let summary = std::fs::read_to_string(&files[0]).unwrap();
    assert!(summary.contains("only_alerts_pixels,12400"), "{summary}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn glad_and_radd_rasters() {
    let d = dir().join("rasters");
    std::fs::create_dir_all(&d).unwrap();
    // a tile of 10 m pixels over the grid's top-left 3 x 3 km: an alert in its top-left quarter, on 2020-02-04
    let tile = grid(10.0, 300, 300);
    let inside = |i: usize| i % 300 < 150 && i / 300 < 150;
    let day = days_from_civil(2020, 2, 4);
    let glad_conf: Vec<f32> = (0..90000).map(|i| if inside(i) { 4.0 } else { 0.0 }).collect();
    let glad_date: Vec<f32> =
        (0..90000).map(|i| if inside(i) { (day - days_from_civil(2018, 12, 31)) as f32 } else { 0.0 }).collect();
    let radd: Vec<f32> = (0..90000)
        .map(|i| if inside(i) { (30000 + day - days_from_civil(2014, 12, 31)) as f32 } else { 0.0 })
        .collect();
    let p = |n: &str| d.join(n).to_string_lossy().into_owned();
    alerts::write_f32(&p("alert.tif"), &tile, &glad_conf).unwrap();
    alerts::write_f32(&p("alertDate.tif"), &tile, &glad_date).unwrap();
    alerts::write_f32(&p("radd.tif"), &tile, &radd).unwrap();
    let g = grid(30.0, 300, 200);
    let change = change_map(&g);
    let year = CompareOptions { from: days_from_civil(2020, 1, 1), to: days_from_civil(2020, 12, 31), bin_days: 30 };
    for (product, file) in [
        (Product::GladS2, RasterFile { conf: p("alert.tif"), date: p("alertDate.tif"), year: 0 }),
        (Product::Radd, RasterFile { conf: p("radd.tif"), date: String::new(), year: 0 }),
    ] {
        let a = alerts::alerts_from_rasters(product, &[file], &g, 1, &|_| {}).unwrap();
        // the tile covers 100 x 100 pixels of the grid, its alerts 50 x 50
        assert_eq!(a.covered.iter().filter(|&&c| c).count(), 10000, "{product:?}");
        let c = alerts::compare(&change, &a, &year);
        assert_eq!((c.covered_pixels, c.both, c.only_map, c.only_alerts), (10000, 2500, 7500, 0), "{product:?}");
        assert_eq!((c.alerts, c.alerts_detected, c.map_patches, c.map_patches_confirmed), (1, 1, 1, 1));
        assert_eq!(c.units[0].unit.day, day);
        assert_eq!(c.units[0].unit.class as usize, product.classes().len() - 1);
        assert_eq!((c.lag.n, c.lag.median), (2500, 26.0));
        assert_eq!(c.classes.last().unwrap().alerts, 1);
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// The s2ro series' box (Rondônia: 63.00 to 62.92 W, 10.00 to 9.92 S) on a grid of 0.0001 degree.
fn rondonia() -> Grid {
    Grid {
        w: 800,
        h: 800,
        gt: [-63.0, 0.0001, 0.0, -9.92, 0.0, -0.0001],
        has_gt: true,
        wkt: SpatialRef::from_epsg(4326).unwrap().to_wkt().unwrap(),
    }
}

/// The live sources over Rondônia: UMD's GLAD-L and GLAD-S2 GeoTIFFs (keyless, read over the network), GFW's
/// RADD and integrated tiles, INPE's DETER (WFS). Their alerts' counts and dates printed.
#[test]
#[ignore]
fn live_sources_over_rondonia() {
    gdal::DriverManager::register_all();
    let g = rondonia();
    let b = alerts::lonlat_bounds(&g).unwrap();
    let (from, to) = (days_from_civil(2019, 1, 1), days_from_civil(2026, 12, 31));
    for p in Product::ALL {
        let files = alerts::remote_files(p, b, from, to).unwrap();
        let t = std::time::Instant::now();
        let a = alerts::alerts_from_rasters(p, &files, &g, 2, &|_| {}).unwrap();
        let (d0, d1, n) = a.days_in(from, to);
        println!(
            "{}: {} files, {n} alert pixels, {} to {}, {:.1} s",
            p.name(),
            files.len(),
            alerts::ymd(d0),
            alerts::ymd(d1),
            t.elapsed().as_secs_f64()
        );
        assert!(a.covered.iter().any(|&c| c), "{p:?} covers nothing");
        assert!(n == 0 || d0 >= days_from_civil(2015, 1, 1) && d1 <= days_from_civil(2027, 1, 1));
    }
    let req = FeatureRequest {
        service: Service::Wfs,
        url: "https://terrabrasilis.dpi.inpe.br/geoserver/deter-amz/ows".into(),
        layer: "deter-amz:deter_amz".into(),
        bbox: Some(b),
        date_field: "view_date".into(),
        ..Default::default()
    };
    let out = dir().join("deter_live.gpkg");
    let f = ogc::fetch_features(&req, &out, &|_, _| {}, &AtomicBool::new(false)).unwrap();
    let a = alerts::alerts_from_vector(&out.to_string_lossy(), &f.layer, "view_date", "classname", &g).unwrap();
    println!("DETER: {} polygons, classes {:?}", a.units.len(), a.classes);
}
