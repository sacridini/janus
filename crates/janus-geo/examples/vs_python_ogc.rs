//! Janus's side of tools/bench_ogc_vs_python.py: features of a WFS or OGC API layer fetched into a GeoPackage, or a
//! WMS tile read as the map reads it. Prints JSON.
//!
//!     vs_python_ogc features SERVICE URL LAYER BBOX|- DATE_FIELD|- FROM|- TO|- MAX OUT.gpkg
//!     vs_python_ogc tile URL LAYER TIME|- Z X Y OUT.rgba
//!
//! BBOX: lon0,lat0,lon1,lat1. SERVICE: wfs or oapif.

use std::sync::atomic::AtomicBool;
use std::time::Instant;

use janus_geo::basemap;
use janus_geo::ogc::{self, FeatureRequest, MapLayerSpec, Service};

fn opt(s: &str) -> String {
    if s == "-" { String::new() } else { s.to_string() }
}

fn main() {
    gdal::DriverManager::register_all();
    let a: Vec<String> = std::env::args().skip(1).collect();
    match a.first().map(String::as_str) {
        Some("features") if a.len() == 10 => {
            let bbox = (a[4] != "-").then(|| {
                let v: Vec<f64> = a[4].split(',').map(|x| x.parse().expect("a number")).collect();
                [v[0], v[1], v[2], v[3]]
            });
            let req = FeatureRequest {
                service: Service::from_id(&a[1]).expect("wfs or oapif"),
                url: a[2].clone(),
                layer: a[3].clone(),
                bbox,
                date_field: opt(&a[5]),
                from: opt(&a[6]),
                to: opt(&a[7]),
                filter: String::new(),
                max_features: a[8].parse().expect("a count"),
                capabilities: None,
            };
            let out = std::path::PathBuf::from(&a[9]);
            let _ = std::fs::remove_file(&out);
            let t = Instant::now();
            match ogc::fetch_features(&req, &out, &|_, _| {}, &AtomicBool::new(false)) {
                Ok(f) => println!(
                    "{{\"features\": {}, \"matched\": {}, \"capped\": {}, \"seconds\": {:.3}}}",
                    f.features,
                    f.matched.map_or("null".to_string(), |m| m.to_string()),
                    f.capped,
                    t.elapsed().as_secs_f64()
                ),
                Err(e) => {
                    println!("{{\"error\": {:?}}}", e);
                    std::process::exit(1);
                }
            }
        }
        Some("tile") if a.len() == 8 => {
            let caps = ogc::capabilities(&a[1], Some(Service::Wms)).expect("the WMS capabilities");
            // a workspace's WMS may list its layers without the workspace's prefix
            let short = a[2].rsplit(':').next().unwrap_or(&a[2]);
            let l = caps
                .layers
                .iter()
                .find(|l| l.name == a[2])
                .or_else(|| caps.layers.iter().find(|l| l.name.rsplit(':').next() == Some(short)))
                .expect("the layer");
            let spec = MapLayerSpec {
                service: Service::Wms,
                url: caps.url.clone(),
                version: if caps.version.starts_with("1.3") { "1.3.0".into() } else { "1.1.1".into() },
                layer: l.name.clone(),
                format: "image/png".into(),
                transparent: true,
                time: opt(&a[3]),
                srs: l.web_mercator_code().into(),
                title: l.label().to_string(),
                ..Default::default()
            };
            let (z, x, y) = (a[4].parse().unwrap(), a[5].parse().unwrap(), a[6].parse().unwrap());
            let dir = std::env::temp_dir().join("janus-vs-python-ogc");
            let _ = std::fs::remove_dir_all(&dir); // no tile from the disk cache
            let t = Instant::now();
            match basemap::read_one_tile(&ogc::map_source(&spec), &dir, z, x, y) {
                Ok(rgba) => {
                    let s = t.elapsed().as_secs_f64();
                    std::fs::write(&a[7], &rgba).expect("the tile written");
                    println!(
                        "{{\"seconds\": {s:.3}, \"layer\": {:?}, \"srs\": {:?}, \"version\": {:?}}}",
                        spec.layer, spec.srs, spec.version
                    );
                }
                Err(e) => {
                    println!("{{\"error\": {:?}}}", e);
                    std::process::exit(1);
                }
            }
        }
        _ => {
            eprintln!("vs_python_ogc features SERVICE URL LAYER BBOX|- DATE_FIELD|- FROM|- TO|- MAX OUT.gpkg");
            eprintln!("vs_python_ogc tile URL LAYER TIME|- Z X Y OUT.rgba");
            std::process::exit(2);
        }
    }
}
