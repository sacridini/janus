//! OGC web layers against the mock server on 127.0.0.1 (janus_geo::ogc_mock; no network): capabilities of WMS,
//! WFS and OGC API - Features (the service found when the URL does not say), features of a box, a date range and a
//! cap fetched into a GeoPackage through GDAL's WFS and OAPIF drivers (paging, the attributes, the CRS, the cache,
//! cancel), a WMS layer's tiles read by the basemap's machinery (each a GetMap of its Web Mercator box, the same
//! image as a direct request) and the errors a user meets (no server, a server error, an unknown layer).

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use gdal::spatial_ref::{AxisMappingStrategy, CoordTransform, SpatialRef};
use gdal::vector::{Geometry, LayerAccess};
use janus_cube::CubeInfo;
use janus_geo::basemap::Basemap;
use janus_geo::ogc::{self, FeatureRequest, MapLayerSpec, Service};
use janus_geo::ogc_mock::{MockData, MockServer, map_colour};

const E: f64 = 20037508.342789244;

/// GDAL's drivers registered once (GDALAllRegister run by two threads at once corrupts the heap).
fn setup() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(gdal::DriverManager::register_all);
}

/// 50 × 30 squares of 120 m every 180 m over the self-test's A (UTM 23S): 1500 features.
fn big() -> MockData {
    MockData { cell: 180.0, side: 120.0, nx: 50, ny: 30, ..Default::default() }
}

/// A UTM 23S box in longitude and latitude (it is at the zone's central meridian: the box stays a rectangle).
fn lonlat_of(b: [f64; 4]) -> [f64; 4] {
    let mut a = SpatialRef::from_epsg(32723).unwrap();
    let mut l = SpatialRef::from_epsg(4326).unwrap();
    a.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    l.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    CoordTransform::new(&a, &l).unwrap().transform_bounds(&b, 21).unwrap()
}

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("janus-ogc-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn wfs_features_of_a_box_paged_capped_cached() {
    setup();
    let srv = MockServer::start(big()).unwrap();
    let url = srv.url("/ows");
    // the service found (OGC API first: not there), the layer listed
    let caps = ogc::capabilities(&url, None).unwrap();
    assert_eq!(caps.service, Service::Wfs);
    assert!(caps.paging);
    assert_eq!(caps.layers.len(), 1);
    let l = &caps.layers[0];
    assert_eq!((l.name.as_str(), l.title.as_str()), ("test:plots", "Test plots"));
    assert!(l.bbox.is_some());
    let fields = ogc::layer_fields(Service::Wfs, &url, "test:plots").unwrap();
    let names: Vec<&str> = fields.iter().map(|f| f.0.as_str()).collect();
    for f in ["name", "day", "area_ha"] {
        assert!(names.contains(&f), "{names:?}");
    }

    let dir = tmp("wfs");
    let none = AtomicBool::new(false);
    // the whole layer: two pages of 1000
    srv.clear_log();
    let req =
        FeatureRequest { service: Service::Wfs, url: url.clone(), layer: "test:plots".into(), ..Default::default() };
    let t = Instant::now();
    let r = ogc::fetch_features(&req, &dir.join("all.gpkg"), &|_, _| {}, &none).unwrap();
    println!("whole layer: {} features in {:.2} s", r.features, t.elapsed().as_secs_f64());
    assert_eq!((r.features, r.capped, r.matched), (1500, false, Some(1500)));
    let pages = srv
        .requests()
        .iter()
        .filter(|q| q.contains("GetFeature") && q.to_ascii_uppercase().contains("STARTINDEX="))
        .count();
    assert!(pages >= 2, "paged requests: {:?}", srv.requests());
    let asked = |srv: &MockServer| srv.requests().iter().filter(|q| q.contains("GetCapabilities")).count();
    assert!(asked(&srv) >= 1, "GDAL's driver asks for the capabilities: {:?}", srv.requests());

    // the capabilities the dialog read given to the driver: not asked again, the same features
    srv.clear_log();
    let with = FeatureRequest { capabilities: caps.raw_caps(), ..req.clone() };
    assert!(with.capabilities.is_some());
    let fields2 = ogc::layer_fields_with(Service::Wfs, &url, "test:plots", caps.raw_caps()).unwrap();
    assert_eq!(fields2, fields);
    let r2 = ogc::fetch_features(&with, &dir.join("all_caps.gpkg"), &|_, _| {}, &none).unwrap();
    assert_eq!((r2.features, r2.matched), (1500, Some(1500)));
    assert_eq!(asked(&srv), 0, "no GetCapabilities with them: {:?}", srv.requests());
    assert_eq!(with.key(), req.key());

    // the GeoPackage: the attributes, the squares, the CRS, the request kept
    {
        let ds = gdal::Dataset::open(&r.path).unwrap();
        let mut lay = ds.layer_by_name(&r.layer).unwrap();
        assert_eq!(lay.feature_count(), 1500);
        let srs = lay.spatial_ref().unwrap();
        assert_eq!(srs.auth_code().ok(), Some(32723));
        let meta = gdal::Metadata::metadata_item(&lay, "JANUS_SOURCE", "").unwrap_or_default();
        assert!(meta.contains("test:plots"), "{meta}");
        let f = lay.features().next().unwrap();
        assert_eq!(f.field_as_string(f.field_index("name").unwrap()).unwrap().as_deref(), Some("plot 1"));
        assert!(f.field_as_string(f.field_index("day").unwrap()).unwrap().unwrap().starts_with("2020"));
        assert!((f.field_as_double(f.field_index("area_ha").unwrap()).unwrap().unwrap() - 1.44).abs() < 1e-9);
        let g: &Geometry = f.geometry().unwrap();
        assert!((g.area() - 14400.0).abs() < 1e-6, "{}", g.area());
    }

    // a box of 10 × 10 cells (its edges between squares): those 100 only
    let bb = lonlat_of([500000.0, 7500000.0 - 1800.0, 500000.0 + 1800.0, 7500000.0]);
    let boxed = FeatureRequest { bbox: Some(bb), ..req.clone() };
    let r = ogc::fetch_features(&boxed, &dir.join("box.gpkg"), &|_, _| {}, &none).unwrap();
    assert_eq!((r.features, r.capped), (100, false));
    assert!(srv.requests().iter().any(|q| q.contains("FILTER") || q.contains("BBOX")));

    // capped: 250 of 1500, said so
    let capped = FeatureRequest { max_features: 250, ..req.clone() };
    let r = ogc::fetch_features(&capped, &dir.join("cap.gpkg"), &|_, _| {}, &none).unwrap();
    assert_eq!((r.features, r.capped), (250, true));

    // cached: the second time at once, nothing asked of the server
    let first = ogc::fetch_cached(&boxed, &dir, 3600, &|_, _| {}, &none).unwrap();
    assert!(!first.cached);
    srv.clear_log();
    let t = Instant::now();
    let again = ogc::fetch_cached(&boxed, &dir, 3600, &|_, _| {}, &none).unwrap();
    assert!(again.cached && again.features == 100 && again.path == first.path);
    assert!(srv.requests().is_empty() && t.elapsed() < Duration::from_secs(2));

    // cancelled
    let stop = AtomicBool::new(true);
    assert_eq!(ogc::fetch_features(&req, &dir.join("x.gpkg"), &|_, _| {}, &stop).unwrap_err(), "cancelled");
    assert!(!dir.join("x.gpkg").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn oapif_features_with_dates_and_pages() {
    setup();
    let data = big();
    let srv = MockServer::start(data.clone()).unwrap();
    let url = srv.url("/oapi");
    let caps = ogc::capabilities(&url, None).unwrap();
    assert_eq!(caps.service, Service::Oapif);
    assert_eq!(caps.layers[0].name, "parcels");
    assert_eq!(caps.layers[0].title, "Test parcels");
    let dir = tmp("oapif");
    let none = AtomicBool::new(false);
    // the days 2020-01-10 to 2020-01-19 over the whole layer: ids 10-19 of each 366
    let req = FeatureRequest {
        service: Service::Oapif,
        url: url.clone(),
        layer: "parcels".into(),
        date_field: "day".into(),
        from: "2020-01-10".into(),
        to: "2020-01-19".into(),
        ..Default::default()
    };
    let want = data.features().iter().filter(|f| ("2020-01-10".."2020-01-20").contains(&f.day.as_str())).count();
    let r = ogc::fetch_features(&req, &dir.join("dates.gpkg"), &|_, _| {}, &none).unwrap();
    assert_eq!(r.features, want, "{:#?}", srv.requests());
    assert!(srv.requests().iter().any(|q| q.contains("datetime=")), "the dates sent to the server");
    // the whole layer: pages of at most 400 through the next links
    let all =
        FeatureRequest { service: Service::Oapif, url: url.clone(), layer: "parcels".into(), ..Default::default() };
    let r = ogc::fetch_features(&all, &dir.join("all.gpkg"), &|_, _| {}, &none).unwrap();
    assert_eq!((r.features, r.capped), (1500, false));
    assert!(srv.requests().iter().filter(|q| q.contains("offset=")).count() >= 3, "next links followed");
    // a box: 10 × 10 cells
    let bb = lonlat_of([500000.0, 7500000.0 - 1800.0, 500000.0 + 1800.0, 7500000.0]);
    let boxed =
        FeatureRequest { service: Service::Oapif, url, layer: "parcels".into(), bbox: Some(bb), ..Default::default() };
    let r = ogc::fetch_features(&boxed, &dir.join("box.gpkg"), &|_, _| {}, &none).unwrap();
    assert_eq!(r.features, 100);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn wms_tiles_are_getmaps_of_their_boxes() {
    setup();
    let srv = MockServer::start(MockData::default()).unwrap();
    let url = srv.url("/ows");
    let caps = ogc::capabilities(&url, Some(Service::Wms)).unwrap();
    assert_eq!(caps.layers.len(), 2);
    let l = caps.layers.iter().find(|l| l.name == "test:alerts").unwrap();
    assert_eq!(l.times.len(), 3);
    assert_eq!(l.attribution, "Janus mock data");
    let spec = MapLayerSpec {
        service: Service::Wms,
        url: caps.url.clone(),
        version: caps.version.clone(),
        layer: l.name.clone(),
        style: String::new(),
        format: "image/png".into(),
        transparent: true,
        time: ogc::time_at(&l.times, "2020-01-20").unwrap(),
        srs: l.web_mercator_code().into(),
        title: l.title.clone(),
        attribution: l.attribution.clone(),
        ..Default::default()
    };
    assert_eq!(spec.time, "2020-01-01");
    let src = ogc::map_source(&spec);
    assert_eq!(src.cache_days, 0, "a local server's tiles are not cached");
    // the self-test's A: 300 × 200 px of 30 m in UTM 23S
    let info = CubeInfo {
        id: 11,
        width: 300,
        height: 200,
        geo_transform: Some([500000.0, 30.0, 0.0, 7500000.0, 0.0, -30.0]),
        crs_wkt: SpatialRef::from_epsg(32723).unwrap().to_wkt().unwrap(),
        crs_authority: "EPSG:32723".into(),
        ..Default::default()
    };
    let dir = tmp("wms");
    let mut bm: Basemap<Vec<u8>> = Basemap::new(&src, &dir, Arc::new(|| {}));
    bm.set_map(&info, None, |_, _, _| Vec::new()).unwrap();
    srv.clear_log();
    let view = [0.0, 0.0, 300.0, 200.0];
    let mut tiles: Vec<Vec<u8>> = Vec::new();
    let start = Instant::now();
    loop {
        bm.tick();
        bm.update(&view, 1.0);
        bm.upload_ready(64, |rgba, _| {
            tiles.push(rgba.to_vec());
            rgba.to_vec()
        });
        if bm.loads() > 0 && bm.inflight() == 0 {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(60), "tiles not read: {}", bm.error());
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(bm.error().is_empty() && !bm.offline(), "{}", bm.error());
    let maps: Vec<String> = srv.requests().into_iter().filter(|q| q.to_ascii_lowercase().contains("getmap")).collect();
    println!("{} tiles, {} GetMap requests", tiles.len(), maps.len());
    assert_eq!(tiles.len(), maps.len());
    for q in &maps {
        let up = q.to_ascii_uppercase();
        assert!(up.contains("LAYERS=TEST:ALERTS") || up.contains("LAYERS=TEST%3AALERTS"), "{q}");
        assert!(up.contains("TIME=2020-01-01"), "{q}");
        assert!(up.contains("EPSG:3857") || up.contains("EPSG%3A3857"), "{q}");
        // a tile's box: a square of the zoom levels' grid
        let target = q.split_whitespace().nth(1).unwrap();
        let (_, query) = target.split_once('?').unwrap();
        let bbox: Vec<f64> = query
            .split('&')
            .find(|kv| kv.to_ascii_uppercase().starts_with("BBOX="))
            .map(|kv| kv[5..].replace("%2C", ",").split(',').filter_map(|v| v.parse().ok()).collect())
            .unwrap();
        let side = bbox[2] - bbox[0];
        let z = (2.0 * E / side).log2().round();
        assert!(((2.0 * E / 2f64.powf(z)) - side).abs() < 1e-3, "{bbox:?}");
        assert!((((bbox[0] + E) / side).round() * side - (bbox[0] + E)).abs() < 1e-3, "{bbox:?}");
        // the tile read is the image of a direct request
        let direct = ogc::get(&srv.url(target), false).unwrap();
        let name = format!("/vsimem/direct_{}.png", tiles.len());
        gdal::vsi::create_mem_file(&name, direct).unwrap();
        let ds = gdal::Dataset::open(&name).unwrap();
        let px: Vec<u8> = (1..=4)
            .map(|b| ds.rasterband(b).unwrap().read_as::<u8>((0, 0), (1, 1), (1, 1), None).unwrap().data()[0])
            .collect();
        drop(ds);
        let _ = gdal::vsi::unlink_mem_file(&name);
        let want = map_colour([bbox[0], bbox[1], bbox[2], bbox[3]], "2020-01-01");
        assert_eq!(px, want.to_vec());
        assert!(tiles.iter().any(|t| t[..4] == want), "a tile of {bbox:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn wms_feature_info_of_a_point() {
    setup();
    let srv = MockServer::start(MockData::default()).unwrap();
    let caps = ogc::capabilities(&srv.url("/ows"), Some(Service::Wms)).unwrap();
    let l = caps.layers.iter().find(|l| l.name == "test:alerts").unwrap();
    assert!(l.queryable);
    // GeoJSON before plain text, GML not read
    assert_eq!(ogc::pick_info_format(&caps.info_formats), "application/json");
    assert_eq!(ogc::pick_info_format(&["application/vnd.ogc.gml".into()]), "");
    let mut spec = MapLayerSpec {
        service: Service::Wms,
        url: caps.url.clone(),
        version: caps.version.clone(),
        layer: l.name.clone(),
        format: "image/png".into(),
        time: "2020-01-01".into(),
        srs: l.web_mercator_code().into(),
        info_format: ogc::pick_info_format(&caps.info_formats),
        info_url: caps.info_url.clone(),
        ..Default::default()
    };
    // the middle of the third square
    let f = &srv.data.features()[2];
    let n = (f.lonlat.len() - 1) as f64;
    let (lon, lat) = f.lonlat[..f.lonlat.len() - 1].iter().fold((0.0, 0.0), |a, p| (a.0 + p[0] / n, a.1 + p[1] / n));
    srv.clear_log();
    let got = ogc::feature_info(&spec, lon, lat, 2.0).unwrap();
    assert_eq!(got.len(), 1, "{got:?}");
    let field =
        |g: &ogc::FeatureInfo, k: &str| g.fields.iter().find(|f| f.0 == k).map(|f| f.1.clone()).unwrap_or_default();
    assert_eq!(got[0].id, "alerts.3");
    assert_eq!((field(&got[0], "name"), field(&got[0], "time")), ("plot 3".into(), "2020-01-01".into()));
    let log = srv.requests();
    assert!(log.iter().any(|q| q.contains("GetFeatureInfo") && q.contains("QUERY_LAYERS=test")), "{log:?}");
    // in plain text (GeoServer's): the same fields, the geometry left out
    spec.info_format = "text/plain".into();
    let got = ogc::feature_info(&spec, lon, lat, 2.0).unwrap();
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(field(&got[0], "name"), "plot 3");
    assert_eq!(field(&got[0], "classname"), "DEGRADACAO");
    assert!(got[0].fields.iter().all(|f| f.0 != "the_geom"), "{got:?}");
    // between the squares: nothing
    let d = &srv.data;
    let ct = {
        let mut a = SpatialRef::from_epsg(d.epsg).unwrap();
        let mut b = SpatialRef::from_epsg(4326).unwrap();
        a.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
        b.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
        CoordTransform::new(&a, &b).unwrap()
    };
    let (mut x, mut y) = ([d.x0 + d.cell], [d.y0 - d.cell / 2.0]);
    ct.transform_coords(&mut x, &mut y, &mut []).unwrap();
    assert!(ogc::feature_info(&spec, x[0], y[0], 1.0).unwrap().is_empty());
    // a layer not queried
    spec.info_format.clear();
    assert!(ogc::feature_info_url(&spec, lon, lat, 2.0).is_none());
}

#[test]
fn errors_are_plain() {
    setup();
    // no server there
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = free.local_addr().unwrap().port();
    drop(free);
    let e = ogc::capabilities(&format!("http://127.0.0.1:{port}/ows"), None).unwrap_err();
    assert!(e.starts_with("no connection"), "{e}");
    let srv = MockServer::start(MockData::default()).unwrap();
    // a server error, a page that is not a service, a bad URL
    let e = ogc::capabilities(&srv.url("/error"), Some(Service::Wms)).unwrap_err();
    assert!(e.contains("500"), "{e}");
    let e = ogc::capabilities(&srv.url("/nothing"), None).unwrap_err();
    assert!(e.contains("no OGC service"), "{e}");
    assert!(ogc::capabilities("ftp://x", None).unwrap_err().contains("https://"));
    // a layer the server does not have
    let none = AtomicBool::new(false);
    let req =
        FeatureRequest { service: Service::Wfs, url: srv.url("/ows"), layer: "test:nope".into(), ..Default::default() };
    let e = ogc::fetch_features(&req, &tmp("err").join("x.gpkg"), &|_, _| {}, &none).unwrap_err();
    assert!(e.contains("test:nope") || e.contains("127.0.0.1"), "{e}");
}
