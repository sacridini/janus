//! What a composite of a STAC series reads (janus_zeit::composite) against every date's bands: the time and the
//! bytes GDAL downloads (its network statistics), for the best available pixel (the quality band of every date,
//! the bands of the winners), the greenest, and the medoid (every date's bands, what a download of the series reads).
//!
//!   cargo run --release -p janus-zeit --example composite_vs_download -- [CATALOG COLLECTION W,S,E,N START END CLOUD [METHODS]]
//!
//! By default the STAC guide's square of Rondonia, Sentinel-2 C1 L2A on Earth Search, June to September 2024, at
//! most 70% of clouds; METHODS (medoid,greenest,bap by default) the runs, their rows replaced in the JSON. GDAL's caches
//! are emptied between the runs; 4 threads. Writes bench/composite-vs-download.json.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use janus_cube::stac::{self, CATALOGS, Search};
use janus_cube::stac_vrt::{self, BuildSpec};
use janus_zeit::composite::{CompOptions, Method, composite};

fn network_bytes() -> u64 {
    // SAFETY: GDAL's network statistics as JSON, freed with VSIFree.
    let json = unsafe {
        let p = gdal_sys::VSINetworkStatsGetAsSerializedJSON(std::ptr::null_mut());
        if p.is_null() {
            return 0;
        }
        let s = std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned();
        gdal_sys::VSIFree(p as *mut std::ffi::c_void);
        s
    };
    let v: serde_json::Value = serde_json::from_str(&json).unwrap_or_default();
    v.pointer("/methods/GET/downloaded_bytes").and_then(|b| b.as_u64()).unwrap_or(0)
}

fn reset() {
    // SAFETY: GDAL's process-wide caches and counters.
    unsafe {
        gdal_sys::VSICurlClearCache();
        gdal_sys::VSINetworkStatsReset();
    }
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let arg = |i: usize, d: &str| a.get(i).cloned().unwrap_or_else(|| d.to_string());
    let url = CATALOGS.iter().find(|c| c.id == arg(0, "earth-search")).map_or(arg(0, ""), |c| c.url.to_string());
    let b: Vec<f64> = arg(2, "-63.05,-9.95,-62.95,-9.85").split(',').map(|v| v.parse().unwrap()).collect();
    let search = Search {
        url: url.clone(),
        collection: arg(1, "sentinel-2-c1-l2a"),
        bbox: [b[0], b[1], b[2], b[3]],
        start: arg(3, "2024-06-01"),
        end: arg(4, "2024-09-30"),
        max_cloud: Some(arg(5, "70").parse().unwrap()),
        max_items: 2000,
    };
    gdal::config::set_config_option("CPL_VSIL_NETWORK_STATS_ENABLED", "YES").unwrap();
    let cancel = AtomicBool::new(false);
    let items = stac::search(&search, &|_| {}, &cancel).expect("search");
    let choices = stac::band_choices(&items);
    let preset = CATALOGS.iter().flat_map(|c| c.collections.iter()).find(|c| c.id == search.collection);
    let keys = stac::default_bands(&choices, preset.map(|p| p.bands));
    let bands: Vec<_> = keys.iter().filter_map(|k| choices.iter().find(|c| &c.key == k).cloned()).collect();
    let dir = std::env::temp_dir().join("janus-composite-bench");
    let _ = std::fs::remove_dir_all(&dir);
    let spec = BuildSpec {
        items: items.clone(),
        bands: bands.clone(),
        bbox: search.bbox,
        res: None,
        dir: dir.join("vrt"),
        planetary: stac::is_planetary(&url),
    };
    let built = stac_vrt::build(&spec, &|_, _| {}, &cancel).expect("build");
    let info = Arc::new(janus_cube::open_cube(&built.files, &janus_cube::BandSelection::default()).expect("open"));
    let groups = stac::group_by_day(&items);
    println!(
        "{} scenes, {} dates, {} bands ({}), {} x {} cells of {} m",
        items.len(),
        info.t(),
        bands.len(),
        keys.join(", "),
        built.grid.w,
        built.grid.h,
        built.grid.gt[1]
    );
    let (mut idx, mut names, mut qa, mut rule) = (Vec::new(), Vec::new(), 0, janus_cube::QaRule::None);
    for (i, b) in bands.iter().enumerate() {
        if b.quality != janus_cube::QaRule::None {
            (qa, rule) = (i as i32 + 1, b.quality);
        } else {
            idx.push(i as i32 + 1);
            names.push(b.label.clone());
        }
    }
    let pos = |p: &dyn Fn(&str) -> bool| bands.iter().position(|b| p(&b.label)).map_or(0, |i| i as i32 + 1);
    let base = CompOptions {
        bands: idx,
        names,
        qa_band: qa,
        qa_rule: rule,
        red: pos(&|l| l == "Red" || l.starts_with("Red (")),
        nir: pos(&|l| l == "NIR" || l.starts_with("NIR (")),
        cover: groups.iter().map(|g| g.cloud.map_or(1.0, |c| 1.0 - c / 100.0)).collect(),
        sensor: vec![1.0; groups.len()],
        threads: 4,
        ..Default::default()
    };
    let f = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench/composite-vs-download.json");
    let old: serde_json::Value =
        std::fs::read_to_string(&f).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let mut rows: Vec<serde_json::Value> = old["runs"].as_array().cloned().unwrap_or_default();
    let methods: Vec<Method> = arg(6, "medoid,greenest,bap")
        .split(',')
        .map(|id| *Method::ALL.iter().find(|m| m.id() == id).expect("a method: medoid, greenest, bap or median"))
        .collect();
    for m in methods {
        reset();
        let t0 = Instant::now();
        let opts = CompOptions { method: m, ..base.clone() };
        let r = composite(&info, &opts, &dir.join(m.id()), &|_, _| {}, &cancel).expect("composite");
        let (s, mb) = (t0.elapsed().as_secs_f64(), network_bytes() as f64 / 1e6);
        println!(
            "{:<24} {s:6.1} s  {mb:8.1} MB downloaded  {:.1} of {:.1} million pixels read",
            m.label(),
            r.reads as f64 / 1e6,
            r.full_reads as f64 / 1e6
        );
        rows.retain(|r| r["method"] != m.id());
        rows.push(serde_json::json!({"method": m.id(), "seconds": s, "megabytes": mb, "pixels_read": r.reads,
            "pixels_of_every_date": r.full_reads}));
    }
    let out = serde_json::json!({
        "what": "a composite of a STAC series: time and bytes downloaded (GDAL's network statistics)",
        "catalog": url, "collection": search.collection, "bbox": search.bbox, "dates": [search.start, search.end],
        "scenes": items.len(), "series_dates": info.t(), "bands": keys, "cells": [built.grid.w, built.grid.h],
        "threads": 4, "runs": rows,
    });
    std::fs::write(&f, serde_json::to_string_pretty(&out).unwrap()).unwrap();
    println!("written {}", f.display());
}
