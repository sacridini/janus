//! OGC web layers from the real servers (network; `cargo test -p janus-geo --test ogc_network -- --ignored
//! --nocapture`): INPE's TerraBrasilis (DETER as WFS and WMS), IBGE's geoservices (municipalities as WFS) and an
//! OGC API - Features server (pygeoapi's demo), NASA's GIBS (a WMTS). Each fetch is small (a box of a few km, a cap) and timed.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use gdal::spatial_ref::SpatialRef;
use janus_cube::CubeInfo;
use janus_geo::basemap::Basemap;
use janus_geo::ogc::{self, FeatureRequest, MapLayerSpec, Service};

fn setup() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(gdal::DriverManager::register_all);
}

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("janus-ogc-net-{name}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A box around Novo Progresso (PA), where DETER has many alerts.
const AMAZON: [f64; 4] = [-55.6, -7.2, -55.2, -6.8];

#[test]
#[ignore]
fn terrabrasilis_deter_wfs() {
    setup();
    let url = "https://terrabrasilis.dpi.inpe.br/geoserver/deter-amz/ows";
    let t = Instant::now();
    let caps = ogc::capabilities(url, Some(Service::Wfs)).unwrap();
    println!("{} layers in {:.1} s ({} KB)", caps.layers.len(), t.elapsed().as_secs_f64(), caps.bytes / 1024);
    let l = caps.layers.iter().find(|l| l.name == "deter-amz:deter_amz").expect("deter_amz listed");
    println!("{}: {}", l.title, l.abstract_text);
    let fields = ogc::layer_fields(Service::Wfs, url, &l.name).unwrap();
    println!("fields: {:?}", fields.iter().map(|f| &f.0).collect::<Vec<_>>());
    assert!(fields.iter().any(|f| f.0 == "view_date"));
    let req = FeatureRequest {
        service: Service::Wfs,
        url: url.into(),
        layer: l.name.clone(),
        bbox: Some(AMAZON),
        date_field: "view_date".into(),
        from: "2023-01-01".into(),
        to: "2023-12-31".into(),
        max_features: 5000,
        ..Default::default()
    };
    let dir = tmp("deter");
    let r = ogc::fetch_features(
        &req,
        &dir.join("deter.gpkg"),
        &|f, m| println!("  {:.0}% {m}", f * 100.0),
        &AtomicBool::new(false),
    )
    .unwrap();
    println!("DETER 2023 in the box: {} features (matched {:?}) in {:.1} s", r.features, r.matched, r.seconds);
    assert!(r.features > 0);
}

#[test]
#[ignore]
fn ibge_municipalities_wfs() {
    setup();
    let url = "https://geoservicos.ibge.gov.br/geoserver/ows";
    let t = Instant::now();
    let caps = ogc::capabilities(url, Some(Service::Wfs)).unwrap();
    println!("IBGE: {} layers in {:.1} s ({} KB)", caps.layers.len(), t.elapsed().as_secs_f64(), caps.bytes / 1024);
    let mun: Vec<&ogc::OgcLayer> = caps.layers.iter().filter(|l| l.matches("municipios")).take(10).collect();
    for l in &mun {
        println!("  {}: {}", l.name, l.title);
    }
    let l = caps
        .layers
        .iter()
        .find(|l| l.name == "CGMAT:pbqg22_02_Municipio_LimUF")
        .or_else(|| mun.first().copied())
        .expect("a layer of municipalities");
    let req = FeatureRequest {
        service: Service::Wfs,
        url: url.into(),
        layer: l.name.clone(),
        bbox: Some(AMAZON),
        max_features: 500,
        ..Default::default()
    };
    let dir = tmp("ibge");
    let r = ogc::fetch_features(&req, &dir.join("mun.gpkg"), &|_, _| {}, &AtomicBool::new(false)).unwrap();
    println!("{}: {} features in {:.1} s", l.name, r.features, r.seconds);
    assert!(r.features > 0);
    // with the capabilities already read (the dialog's): GDAL's driver asks for them no more
    let with = FeatureRequest { capabilities: caps.raw_caps(), ..req.clone() };
    let r2 = ogc::fetch_features(&with, &dir.join("mun_caps.gpkg"), &|_, _| {}, &AtomicBool::new(false)).unwrap();
    println!("with the capabilities: {} features in {:.1} s", r2.features, r2.seconds);
    assert_eq!(r2.features, r.features);
}

#[test]
#[ignore]
fn pygeoapi_oapif() {
    setup();
    let url = "https://demo.pygeoapi.io/master";
    let caps = ogc::capabilities(url, None).unwrap();
    assert_eq!(caps.service, Service::Oapif);
    println!("{} collections: {:?}", caps.layers.len(), caps.layers.iter().map(|l| &l.name).collect::<Vec<_>>());
    let l = caps.layers.iter().find(|l| l.name == "lakes").expect("lakes");
    let req = FeatureRequest { service: Service::Oapif, url: url.into(), layer: l.name.clone(), ..Default::default() };
    let dir = tmp("pygeoapi");
    let r = ogc::fetch_features(&req, &dir.join("lakes.gpkg"), &|_, _| {}, &AtomicBool::new(false)).unwrap();
    println!("lakes: {} features in {:.1} s", r.features, r.seconds);
    assert!(r.features > 0);
}

#[test]
#[ignore]
fn terrabrasilis_deter_wms_tiles() {
    setup();
    let url = "https://terrabrasilis.dpi.inpe.br/geoserver/deter-amz/ows";
    let caps = ogc::capabilities(url, Some(Service::Wms)).unwrap();
    let l = caps.layers.iter().find(|l| l.name.ends_with("deter_amz")).expect("deter_amz");
    println!("{} times, default {}", l.times.len(), l.time_default);
    let spec = MapLayerSpec {
        service: Service::Wms,
        url: caps.url.clone(),
        version: caps.version.clone(),
        layer: l.name.clone(),
        format: "image/png".into(),
        transparent: true,
        srs: l.web_mercator_code().into(),
        title: l.title.clone(),
        ..Default::default()
    };
    // a UTM 21S layer over the box
    let info = CubeInfo {
        id: 3,
        width: 1500,
        height: 1500,
        geo_transform: Some([700000.0, 30.0, 0.0, 9230000.0, 0.0, -30.0]),
        crs_wkt: SpatialRef::from_epsg(32721).unwrap().to_wkt().unwrap(),
        crs_authority: "EPSG:32721".into(),
        ..Default::default()
    };
    let dir = tmp("wms");
    let mut bm: Basemap<Vec<u8>> = Basemap::new(&ogc::map_source(&spec), &dir, Arc::new(|| {}));
    bm.set_map(&info, None, |_, _, _| Vec::new()).unwrap();
    let start = Instant::now();
    let mut opaque = 0usize;
    loop {
        bm.tick();
        bm.update(&[0.0, 0.0, 1500.0, 1500.0], 0.5);
        bm.upload_ready(64, |rgba, _| {
            opaque += rgba.chunks(4).filter(|p| p[3] > 0).count();
            rgba.to_vec()
        });
        if bm.loads() > 0 && bm.inflight() == 0 {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(120), "tiles not read: {}", bm.error());
        std::thread::sleep(Duration::from_millis(20));
    }
    println!(
        "{} tiles in {:.1} s ({:.0} ms each), {opaque} drawn pixels; error {:?}",
        bm.loads(),
        start.elapsed().as_secs_f64(),
        bm.avg_load_ms(),
        bm.error()
    );
    assert!(bm.error().is_empty());
    assert!(opaque > 0, "DETER's polygons drawn somewhere in the box");
}

#[test]
#[ignore]
fn nasa_gibs_wmts_tiles() {
    setup();
    let url = "https://gibs.earthdata.nasa.gov/wmts/epsg3857/best/wmts.cgi";
    let t = Instant::now();
    let caps = ogc::capabilities(url, Some(Service::Wmts)).unwrap();
    let tiled = caps.layers.iter().filter(|l| l.tiles.is_some()).count();
    println!(
        "GIBS: {} layers ({tiled} in Web Mercator tiles) in {:.1} s ({} KB)",
        caps.layers.len(),
        t.elapsed().as_secs_f64(),
        caps.bytes / 1024
    );
    assert!(tiled > 0);
    // the layers with a time ({Time}: its default filled in) leave only the tiles' and the style's names
    let left: Vec<&str> = caps
        .layers
        .iter()
        .filter_map(|l| l.tiles.as_ref())
        .map(|t| t.0.as_str())
        .filter(|t| {
            let rest = ["{z}", "{y}", "{x}", "{Style}", "{Format}"].iter().fold(t.to_string(), |s, k| s.replace(k, ""));
            rest.contains('{')
        })
        .collect();
    assert!(left.is_empty(), "{} templates left unfilled, e.g. {:?}", left.len(), left.first());
    let l = caps
        .layers
        .iter()
        .find(|l| l.name == "BlueMarble_ShadedRelief_Bathymetry" && l.tiles.is_some())
        .or_else(|| caps.layers.iter().find(|l| l.tiles.is_some()))
        .unwrap();
    let (tiles, max_zoom) = l.tiles.clone().unwrap();
    let format = l.formats.iter().find(|f| f.contains("png")).or(l.formats.first()).cloned().unwrap_or_default();
    println!("{}: {tiles} (z 0-{max_zoom}, {format})", l.name);
    let spec = MapLayerSpec {
        service: Service::Wmts,
        url: caps.url.clone(),
        version: caps.version.clone(),
        layer: l.name.clone(),
        style: l.styles.first().cloned().unwrap_or_default(),
        format,
        tiles,
        max_zoom,
        title: l.title.clone(),
        ..Default::default()
    };
    // the same UTM 21S layer as the WMS's
    let info = CubeInfo {
        id: 4,
        width: 1500,
        height: 1500,
        geo_transform: Some([700000.0, 30.0, 0.0, 9230000.0, 0.0, -30.0]),
        crs_wkt: SpatialRef::from_epsg(32721).unwrap().to_wkt().unwrap(),
        crs_authority: "EPSG:32721".into(),
        ..Default::default()
    };
    let dir = tmp("wmts");
    let mut bm: Basemap<Vec<u8>> = Basemap::new(&ogc::map_source(&spec), &dir, Arc::new(|| {}));
    bm.set_map(&info, None, |_, _, _| Vec::new()).unwrap();
    let start = Instant::now();
    let mut opaque = 0usize;
    loop {
        bm.tick();
        bm.update(&[0.0, 0.0, 1500.0, 1500.0], 0.5);
        bm.upload_ready(64, |rgba, _| {
            opaque += rgba.chunks(4).filter(|p| p[3] > 0).count();
            rgba.to_vec()
        });
        if bm.loads() > 0 && bm.inflight() == 0 {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(120), "tiles not read: {}", bm.error());
        std::thread::sleep(Duration::from_millis(20));
    }
    println!(
        "{} tiles in {:.1} s ({:.0} ms each), {opaque} drawn pixels; error {:?}",
        bm.loads(),
        start.elapsed().as_secs_f64(),
        bm.avg_load_ms(),
        bm.error()
    );
    assert!(bm.error().is_empty());
    assert!(opaque > 0, "the relief drawn over the box");
}
