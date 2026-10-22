//! The native raster runs against the Python bridge's (tests/data/raster, written by
//! tools/zeit_raster_fixtures.py): the same spec, the same outputs, pixel for pixel, with the same
//! georeferencing, band descriptions, NoData and scale.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use gdal::{Dataset, Metadata};
use janus_zeit::job::{JobSpec, run_job};
use serde_json::Value;

fn cases() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/raster");
    let mut out: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("spec.json").exists())
        .collect();
    out.sort();
    out
}

/// Where two rasters differ, if they do.
fn compare_rasters(a: &Path, b: &Path) -> Option<String> {
    let (da, db) = (Dataset::open(a).ok()?, Dataset::open(b).ok()?);
    if da.raster_size() != db.raster_size() || da.raster_count() != db.raster_count() {
        return Some(format!(
            "size {:?} x {} vs {:?} x {}",
            da.raster_size(),
            da.raster_count(),
            db.raster_size(),
            db.raster_count()
        ));
    }
    if da.geo_transform().ok() != db.geo_transform().ok() {
        return Some(format!("geotransform {:?} vs {:?}", da.geo_transform().ok(), db.geo_transform().ok()));
    }
    let same_crs = match (da.spatial_ref(), db.spatial_ref()) {
        (Ok(x), Ok(y)) => x.auth_code().ok() == y.auth_code().ok(),
        (Err(_), Err(_)) => true,
        _ => false,
    };
    if !same_crs {
        return Some("CRS".into());
    }
    let (w, h) = da.raster_size();
    for k in 1..=da.raster_count() {
        let (ba, bb) = (da.rasterband(k).ok()?, db.rasterband(k).ok()?);
        if ba.band_type() != bb.band_type() {
            return Some(format!("band {k}: type {:?} vs {:?}", ba.band_type(), bb.band_type()));
        }
        let nd = |v: Option<f64>| v.map(f64::to_bits);
        if nd(ba.no_data_value()) != nd(bb.no_data_value())
            && !(ba.no_data_value().is_some_and(f64::is_nan) && bb.no_data_value().is_some_and(f64::is_nan))
        {
            return Some(format!("band {k}: NoData {:?} vs {:?}", ba.no_data_value(), bb.no_data_value()));
        }
        if ba.description().ok() != bb.description().ok() || ba.scale() != bb.scale() || ba.offset() != bb.offset() {
            return Some(format!(
                "band {k}: description/scale/offset {:?} {:?} {:?} vs {:?} {:?} {:?}",
                ba.description().ok(),
                ba.scale(),
                ba.offset(),
                bb.description().ok(),
                bb.scale(),
                bb.offset()
            ));
        }
        let va = ba.read_as::<f64>((0, 0), (w, h), (w, h), None).ok()?;
        let vb = bb.read_as::<f64>((0, 0), (w, h), (w, h), None).ok()?;
        let mut bad = 0;
        let mut first = String::new();
        for (i, (x, y)) in va.data().iter().zip(vb.data()).enumerate() {
            if x.to_bits() != y.to_bits() && !(x.is_nan() && y.is_nan()) {
                if bad == 0 {
                    first = format!("pixel ({}, {}): {x} vs {y}", i % w, i / w);
                }
                bad += 1;
            }
        }
        if bad > 0 {
            return Some(format!("band {k}: {bad} pixels differ, first {first}"));
        }
    }
    None
}

#[test]
fn raster_runs_match_the_bridge() {
    let tmp = std::env::temp_dir().join("janus-zeit-raster-test");
    let mut failures = vec![];
    let all = cases();
    assert!(!all.is_empty(), "no raster fixtures");
    for case in &all {
        let name = case.file_name().unwrap().to_string_lossy().to_string();
        let text = std::fs::read_to_string(case.join("spec.json")).unwrap();
        let mut spec: JobSpec = serde_json::from_str(&text).unwrap();
        let abs = |p: &str| case.join(p).to_string_lossy().to_string();
        spec.input = abs(&spec.input);
        spec.nd_input = spec.nd_input.as_deref().map(abs);
        spec.qa_input = spec.qa_input.as_deref().map(abs);
        spec.bands = spec.bands.iter().map(|(r, p)| (r.clone(), abs(p))).collect();
        let out = tmp.join(&name);
        let _ = std::fs::remove_dir_all(&out);
        spec.output_dir = out.to_string_lossy().to_string();
        let expected: Value =
            serde_json::from_str(&std::fs::read_to_string(case.join("expected/result.json")).unwrap()).unwrap();
        let got = run_job(&spec, &|_, _| {}, &AtomicBool::new(false));
        match (got, expected.get("error")) {
            (Err(_), Some(_)) => println!("{name}: fails as the bridge"),
            (Ok(r), None) => {
                let mut want = expected["outputs"].as_array().cloned().unwrap_or_default();
                let mut have = r.outputs.clone();
                let paths: Vec<String> = have.iter().map(|o| o["path"].as_str().unwrap_or("").to_string()).collect();
                for o in want.iter_mut().chain(have.iter_mut()) {
                    o.as_object_mut().map(|m| m.remove("path"));
                }
                if want != have {
                    failures.push(format!("{name}: outputs {have:?}\n  bridge {want:?}"));
                    continue;
                }
                for p in paths {
                    let file = Path::new(&p).file_name().unwrap();
                    if let Some(why) = compare_rasters(Path::new(&p), &case.join("expected").join(file)) {
                        failures.push(format!("{name}/{}: {why}", file.to_string_lossy()));
                    }
                }
                if !failures.iter().any(|f| f.starts_with(&name)) {
                    println!("{name}: {} outputs as the bridge", have.len());
                }
            }
            (got, _) => failures.push(format!("{name}: native {:?}, bridge {expected}", got.map(|r| r.outputs))),
        }
    }
    for f in &failures {
        println!("{f}");
    }
    assert!(failures.is_empty(), "{} differences", failures.len());
}
