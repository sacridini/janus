//! Janus's side of tools/bench_alerts_vs_python.py: a map of change dates against alerts, timed (the median of
//! RUNS runs: the map read, the alerts placed on its grid, compared). Prints JSON: the summary, the classes, the
//! lags' histogram, the seconds.
//!
//!     vs_python_alerts vector MAP GPKG LAYER DATE_FIELD CLASS_FIELD|- FROM TO BIN RUNS
//!     vs_python_alerts raster MAP PRODUCT FROM TO BIN RUNS CONF[,DATE[,YEAR]] ...
//!     vs_python_alerts gfw-mosaic PRODUCT LON0,LAT0,LON1,LAT1 OUT.tif      (GFW's tiles of a box as a GeoTIFF)
//!     vs_python_alerts deter URL LAYER LON0,LAT0,LON1,LAT1 FROM TO OUT.gpkg (DETER's polygons of a box)
//!
//! PRODUCT: glad-l, glad-s2, radd, integrated. FROM, TO: YYYY-MM-DD.

use std::sync::atomic::AtomicBool;
use std::time::Instant;

use janus_geo::ogc::{self, FeatureRequest, Service};
use janus_validation::alerts::{self, CompareOptions, Product, RasterFile, parse_day};
use serde_json::json;

fn bbox(s: &str) -> [f64; 4] {
    let v: Vec<f64> = s.split(',').map(|x| x.parse().expect("a number")).collect();
    [v[0], v[1], v[2], v[3]]
}

fn main() {
    gdal::DriverManager::register_all();
    let a: Vec<String> = std::env::args().skip(1).collect();
    let fail = |e: String| -> ! {
        println!("{}", json!({ "error": e }));
        std::process::exit(1)
    };
    match a.first().map(String::as_str) {
        Some("gfw-mosaic") if a.len() == 4 => {
            let p = Product::from_id(&a[1]).expect("a product");
            let files = alerts::remote_files(p, bbox(&a[2]), i32::MIN + 1, i32::MAX).unwrap_or_else(|e| fail(e));
            let t = Instant::now();
            alerts::gfw_mosaic(&files[0].conf, std::path::Path::new(&a[3]), &|_, _| {}).unwrap_or_else(|e| fail(e));
            println!("{}", json!({ "spec": files[0].conf, "seconds": t.elapsed().as_secs_f64() }));
        }
        Some("deter") if a.len() == 7 => {
            let req = FeatureRequest {
                service: Service::Wfs,
                url: a[1].clone(),
                layer: a[2].clone(),
                bbox: Some(bbox(&a[3])),
                date_field: "view_date".into(),
                from: a[4].clone(),
                to: a[5].clone(),
                ..Default::default()
            };
            let out = std::path::PathBuf::from(&a[6]);
            let f = ogc::fetch_features(&req, &out, &|_, _| {}, &AtomicBool::new(false)).unwrap_or_else(|e| fail(e));
            println!("{}", json!({ "features": f.features, "layer": f.layer, "seconds": f.seconds }));
        }
        Some(kind @ ("vector" | "raster")) => {
            let map = &a[1];
            let (o, runs, rest) = if kind == "vector" {
                let o = CompareOptions {
                    from: parse_day(&a[6]).expect("FROM"),
                    to: parse_day(&a[7]).expect("TO"),
                    bin_days: a[8].parse().expect("BIN"),
                };
                (o, a[9].parse::<usize>().expect("RUNS"), 0)
            } else {
                let o = CompareOptions {
                    from: parse_day(&a[3]).expect("FROM"),
                    to: parse_day(&a[4]).expect("TO"),
                    bin_days: a[5].parse().expect("BIN"),
                };
                (o, a[6].parse::<usize>().expect("RUNS"), 7)
            };
            let mut times = Vec::new();
            let mut last = None;
            for _ in 0..runs.max(1) {
                let t = Instant::now();
                let (grid, change) = alerts::read_change_map(map, 1).unwrap_or_else(|e| fail(e));
                let on_grid = if kind == "vector" {
                    let class = if a[5] == "-" { "" } else { a[5].as_str() };
                    alerts::alerts_from_vector(&a[2], &a[3], &a[4], class, &grid).unwrap_or_else(|e| fail(e))
                } else {
                    let p = Product::from_id(&a[2]).expect("a product");
                    let files: Vec<RasterFile> = a[rest..]
                        .iter()
                        .map(|f| {
                            let v: Vec<&str> = f.split(',').collect();
                            RasterFile {
                                conf: v[0].to_string(),
                                date: v.get(1).map_or(String::new(), |s| s.to_string()),
                                year: v.get(2).and_then(|y| y.parse().ok()).unwrap_or(0),
                            }
                        })
                        .collect();
                    alerts::alerts_from_rasters(p, &files, &grid, 1, &|_| {}).unwrap_or_else(|e| fail(e))
                };
                let c = alerts::compare(&change, &on_grid, &o);
                times.push(t.elapsed().as_secs_f64());
                last = Some((c, on_grid));
            }
            let (c, on_grid) = last.unwrap();
            times.sort_by(f64::total_cmp);
            let summary: serde_json::Map<String, serde_json::Value> = c
                .summary()
                .into_iter()
                .map(|(k, v)| (k.to_string(), if v.is_finite() { json!(v) } else { json!(null) }))
                .collect();
            let classes: Vec<_> = c
                .classes
                .iter()
                .map(|k| {
                    json!({
                        "class": k.name, "alerts": k.alerts, "detected": k.detected, "ha": k.ha,
                        "detected_ha": k.detected_ha,
                        "lag_median": if k.lag_median.is_finite() { json!(k.lag_median) } else { json!(null) }
                    })
                })
                .collect();
            println!(
                "{}",
                json!({
                    "grid": [on_grid.grid.w, on_grid.grid.h],
                    "summary": summary,
                    "classes": classes,
                    "histogram": c.histogram,
                    "seconds": times[times.len() / 2],
                    "runs": times,
                })
            );
        }
        _ => {
            eprintln!("usage: see the source's header");
            std::process::exit(2);
        }
    }
}
