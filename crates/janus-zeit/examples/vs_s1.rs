//! Janus's Sentinel-1 path (IDEIAS phase 47) timed for tools/bench_s1_vs_stackstac.py:
//!
//!     vs_s1 OUT_DIR W,S,E,N START END THREADS
//!
//! Planetary Computer's sentinel-1-rtc searched over the box between the dates, the pass with the most scenes
//! kept, its days built as VRTs (VV, VH, 10 m) and read whole as VV in dB (the VVdB index of the library):
//! OUT_DIR/janus_vvdb.f32 (dates x rows x columns, float32). The linear values written as local GeoTIFFs
//! (OUT_DIR/linear/s1_YYYYMMDD.tif, VV and VH) and filtered from them by Quegan's and Lee's filters (7 x 7) into
//! OUT_DIR/quegan and OUT_DIR/lee. Prints {"search_s", "build_s", "read_db_s", "quegan_s", "lee_s", the grid,
//! the dates' items in the order drawn}.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use gdal::Metadata;
use janus_cube::stac::{self, Search};
use janus_cube::stac_vrt::{self, BuildSpec};
use janus_zeit::speckle::{Filter, SpeckleOptions, speckle};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 6 {
        eprintln!("vs_s1 OUT_DIR W,S,E,N START END THREADS");
        std::process::exit(2);
    }
    let fail = |e: String| -> ! {
        eprintln!("{e}");
        std::process::exit(1)
    };
    let out = Path::new(&a[1]);
    std::fs::create_dir_all(out).unwrap_or_else(|e| fail(e.to_string()));
    let b: Vec<f64> = a[2].split(',').filter_map(|v| v.parse().ok()).collect();
    let threads: usize = a.get(5).and_then(|t| t.parse().ok()).unwrap_or(8);
    let none = AtomicBool::new(false);
    let t0 = Instant::now();
    let search = Search {
        url: "https://planetarycomputer.microsoft.com/api/stac/v1".into(),
        collection: "sentinel-1-rtc".into(),
        bbox: [b[0], b[1], b[2], b[3]],
        start: a[3].clone(),
        end: a[4].clone(),
        max_cloud: None,
        max_items: 2000,
    };
    let items = stac::search(&search, &|_| {}, &none).unwrap_or_else(|e| fail(e));
    let orbits = stac::orbits(&items);
    let pass = orbits.first().unwrap_or_else(|| fail("no scenes".into()));
    let groups = stac::group_by_day_of(&items, &pass.items);
    let search_s = t0.elapsed().as_secs_f64();
    let t1 = Instant::now();
    let choices = stac::band_choices(&items);
    let bands: Vec<_> = ["vv", "vh"].iter().filter_map(|k| choices.iter().find(|c| c.key == *k).cloned()).collect();
    let kept: Vec<stac::Item> = groups.iter().flat_map(|g| g.items.iter().map(|&i| items[i].clone())).collect();
    let spec =
        BuildSpec { items: kept, bands, bbox: search.bbox, res: Some(10.0), dir: out.join("vrt"), planetary: true };
    let built = stac_vrt::build(&spec, &|_, _| {}, &none).unwrap_or_else(|e| fail(e));
    let build_s = t1.elapsed().as_secs_f64();
    // VV in dB, every date read whole on the threads
    let ix = janus_cube::calc::indices::find("VVdB").unwrap_or_else(|| fail("no VVdB".into()));
    let sel = janus_cube::BandSelection {
        expr: janus_cube::calc::ExprId::intern(&ix.name, &ix.formula),
        chosen: true,
        ..Default::default()
    };
    let t2 = Instant::now();
    let info = Arc::new(janus_cube::open_cube(&built.files, &sel).unwrap_or_else(|e| fail(e)));
    let (w, h, t_n) = (info.width, info.height, info.t());
    let read_all = |info: &Arc<janus_cube::CubeInfo>, all: bool| -> Vec<f32> {
        let per = if all { info.bands_per_date.max(1) } else { 1 } * w * h;
        let mut v = vec![f32::NAN; t_n * per];
        let next = std::sync::atomic::AtomicUsize::new(0);
        let chunks: Vec<std::sync::Mutex<&mut [f32]>> = v.chunks_mut(per).map(std::sync::Mutex::new).collect();
        std::thread::scope(|s| {
            for _ in 0..threads.min(t_n) {
                s.spawn(|| {
                    loop {
                        let t = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if t >= t_n {
                            break;
                        }
                        let mut c = chunks[t].lock().unwrap();
                        janus_cube::with_thread_reader(info, |r| {
                            if all {
                                for b in 0..info.bands_per_date {
                                    r.read_band_window(
                                        t,
                                        b as i32 + 1,
                                        (0, 0, w, h),
                                        &mut c[b * w * h..(b + 1) * w * h],
                                        (w, h),
                                    );
                                }
                            } else {
                                r.read_window(t, (0, 0, w, h), &mut c, (w, h));
                            }
                        });
                    }
                });
            }
        });
        v
    };
    let db = read_all(&info, false);
    let read_db_s = t2.elapsed().as_secs_f64();
    let bytes: Vec<u8> = db.iter().flat_map(|v| v.to_le_bytes()).collect();
    std::fs::write(out.join("janus_vvdb.f32"), bytes).unwrap_or_else(|e| fail(e.to_string()));
    // the linear values as local GeoTIFFs, then filtered
    let lin = Arc::new(janus_cube::open_cube(&built.files, &Default::default()).unwrap_or_else(|e| fail(e)));
    let values = read_all(&lin, true);
    let dir = out.join("linear");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| fail(e.to_string()));
    let drv = gdal::DriverManager::get_driver_by_name("GTiff").unwrap();
    let mut local = Vec::new();
    for t in 0..t_n {
        let day = &info.layers[t].label;
        let p = dir.join(format!("s1_{}.tif", day.replace('-', "")));
        let mut d = drv.create_with_band_type::<f32, _>(&p, w, h, 2).unwrap();
        d.set_geo_transform(&built.grid.gt).unwrap();
        d.set_projection(&lin.crs_wkt).unwrap();
        for bd in 0..2 {
            let mut rb = d.rasterband(bd + 1).unwrap();
            rb.set_description(["VV", "VH"][bd]).unwrap();
            rb.set_no_data_value(Some(f64::NAN)).unwrap();
            let s = &values[(t * 2 + bd) * w * h..(t * 2 + bd + 1) * w * h];
            let mut buf = gdal::raster::Buffer::new((w, h), s.to_vec());
            rb.write((0, 0), (w, h), &mut buf).unwrap();
        }
        local.push(p.to_string_lossy().into_owned());
    }
    let loc = Arc::new(janus_cube::open_cube(&local, &Default::default()).unwrap_or_else(|e| fail(e)));
    let mut times = serde_json::Map::new();
    for (name, filter) in [("quegan", Filter::Quegan), ("lee", Filter::Lee { looks: 4.4 })] {
        let o = out.join(name);
        let _ = std::fs::remove_dir_all(&o);
        let opts = SpeckleOptions { filter, size: 7, window: None, stem: "f".into(), threads };
        let t = Instant::now();
        speckle(&loc, &opts, &o, &|_, _| {}, &none).unwrap_or_else(|e| fail(e));
        times.insert(format!("{name}_s"), serde_json::json!(t.elapsed().as_secs_f64()));
    }
    let dates: Vec<serde_json::Value> = groups
        .iter()
        .map(|g| {
            let day = janus_cube::dates::format_date(g.day as f64 * 86400.0, janus_cube::TimeGranularity::Day);
            // the order the VRT draws them, the last on top
            let ids: Vec<&str> = g.items.iter().map(|&i| items[i].id.as_str()).collect();
            serde_json::json!({"day": day, "items": ids})
        })
        .collect();
    let mut r = serde_json::json!({
        "search_s": search_s, "build_s": build_s, "read_db_s": read_db_s, "items_found": items.len(),
        "pass": pass.label(), "passes": orbits.iter().map(|o| (o.label(), o.items.len())).collect::<Vec<_>>(),
        "crs": built.grid.crs, "gt": built.grid.gt, "w": w, "h": h, "dates": dates,
        "band_names": info.band_names,
    });
    for (k, v) in times {
        r[k] = v;
    }
    println!("{r}");
}
