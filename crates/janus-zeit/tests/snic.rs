//! The native SNIC against Zeit's (tests/data/snic, written by tools/zeit_snic_fixtures.py): the seeds of R snic's
//! grids, and runs on small series whose labels, sizes and means are zeit.segmentation.run_snic's on the same
//! features; the polygons' GeoPackage, a polygon a segment with its size, centre and means.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use gdal::Dataset;
use gdal::vector::LayerAccess;
use janus_zeit::job::{JobSpec, run_job};
use janus_zeit::snic::grid;
use serde_json::Value;

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/snic")
}

fn json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))).unwrap()
}

fn floats(v: &Value) -> Vec<f64> {
    v.as_array().unwrap().iter().map(|x| x.as_f64().unwrap_or(f64::NAN)).collect()
}

fn pair(v: &Value) -> [f64; 2] {
    let a = floats(v);
    [a[0], a[1]]
}

#[test]
fn grids_are_r_snics() {
    for case in json(&dir().join("grids.json")).as_array().unwrap() {
        let (h, w) = (case["h"].as_u64().unwrap() as usize, case["w"].as_u64().unwrap() as usize);
        let padding = (!case["padding"].is_null()).then(|| pair(&case["padding"]));
        let kind = case["type"].as_str().unwrap();
        let got = grid(h, w, pair(&case["spacing"]), padding, kind, case["seed"].as_u64().unwrap());
        let what = format!("{kind} {h} x {w}");
        match (got, case.get("error").and_then(Value::as_str)) {
            (Err(e), Some(want)) => assert!(e.contains(want), "{what}: {e} vs {want}"),
            (Ok(seeds), None) => {
                let want: Vec<(i64, i64)> = case["seeds"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|s| (s[0].as_i64().unwrap(), s[1].as_i64().unwrap()))
                    .collect();
                assert_eq!(seeds, want, "{what}");
            }
            (got, want) => panic!("{what}: {got:?} vs {want:?}"),
        }
    }
}

fn band(ds: &Dataset, k: usize) -> Vec<f64> {
    let (w, h) = ds.raster_size();
    ds.rasterband(k).unwrap().read_as::<f64>((0, 0), (w, h), (w, h), None).unwrap().data().to_vec()
}

/// The polygons' rows: supercells -> (x, y, n_pixels, means).
fn polygons(path: &Path) -> Vec<(i64, f64, f64, i64, Vec<f64>)> {
    let ds = Dataset::open(path).unwrap();
    let mut layer = ds.layer_by_name("segments").unwrap();
    let names: Vec<String> = layer.defn().fields().map(|f| f.name()).collect();
    layer
        .features()
        .map(|f| {
            let get = |k: usize| f.field_as_double(k).ok().flatten().unwrap_or(f64::NAN);
            let k = f.field_as_integer64(0).unwrap().unwrap();
            let n = f.field_as_integer64(3).unwrap().unwrap();
            (k, get(1), get(2), n, (4..names.len()).map(get).collect())
        })
        .collect()
}

#[test]
fn runs_are_zeits() {
    let tmp = std::env::temp_dir().join("janus-zeit-snic-test");
    let mut cases: Vec<PathBuf> = std::fs::read_dir(dir())
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("spec.json").exists())
        .collect();
    cases.sort();
    assert!(!cases.is_empty(), "no SNIC fixtures");
    for case in &cases {
        let name = case.file_name().unwrap().to_string_lossy().to_string();
        let mut spec: JobSpec =
            serde_json::from_str(&std::fs::read_to_string(case.join("spec.json")).unwrap()).unwrap();
        let abs = |p: &str| case.join(p).to_string_lossy().to_string();
        spec.input = abs(&spec.input);
        spec.bands = spec.bands.iter().map(|(r, p)| (r.clone(), abs(p))).collect();
        let out = tmp.join(&name);
        let _ = std::fs::remove_dir_all(&out);
        spec.output_dir = out.to_string_lossy().to_string();
        let want = json(&case.join("expected.json"));
        let res = run_job(&spec, &|_, _| {}, &AtomicBool::new(false)).unwrap_or_else(|e| panic!("{name}: {e}"));
        let path = |id: &str| {
            let o = res.outputs.iter().find(|o| o["id"] == id).unwrap_or_else(|| panic!("{name}: no {id}"));
            PathBuf::from(o["path"].as_str().unwrap())
        };

        // the labels: zeit's, pixel for pixel
        let labels = Dataset::open(path("labels")).unwrap();
        assert_eq!(labels.rasterband(1).unwrap().band_type(), gdal::raster::GdalDataType::Int32, "{name}");
        let [x0, y0, x1, y1] = res.window;
        assert_eq!(labels.raster_size(), (x1 - x0, y1 - y0), "{name}");
        let got = band(&labels, 1);
        let lab = floats(&want["labels"]);
        let differ = got.iter().zip(&lab).filter(|(a, b)| a != b).count();
        assert_eq!(differ, 0, "{name}: {differ} labels differ from zeit's");
        let gt = labels.geo_transform().unwrap();
        let input = Dataset::open(&spec.input).unwrap().geo_transform().unwrap();
        assert_eq!(gt[0], input[0] + x0 as f64 * input[1], "{name}: the window's origin");

        // the mean series: each pixel its segment's mean of every date
        let sizes = floats(&want["sizes"]);
        let k_n = sizes.len();
        let t = spec.years.len();
        let shown = floats(&want["shown_means"]);
        let mean = Dataset::open(path("mean")).unwrap();
        assert_eq!(mean.raster_count(), t, "{name}");
        for i in 0..t {
            let b = band(&mean, i + 1);
            for (px, &l) in lab.iter().enumerate() {
                let w = if l >= 0.0 { shown[l as usize * t + i] as f32 as f64 } else { f64::NAN };
                assert!(
                    (b[px].is_nan() && w.is_nan()) || (b[px] - w).abs() <= 1e-6 * w.abs().max(1.0),
                    "{name}: date {i}, pixel {px}: {} vs {w}",
                    b[px]
                );
            }
        }

        // the polygons: one a segment, with zeit's sizes, centres (in the map's coordinates) and means
        let rows = polygons(&path("polygons"));
        let non_empty = sizes.iter().filter(|&&s| s > 0.0).count();
        assert_eq!(rows.len(), non_empty, "{name}: polygons");
        let (cen, means) = (floats(&want["centroids"]), floats(&want["means"]));
        let f_n = means.len() / k_n;
        for (k, x, y, n, m) in rows {
            let k = k as usize;
            assert_eq!(n as f64, sizes[k], "{name}: segment {k}'s size");
            let (cx, cy) = (gt[0] + (cen[2 * k + 1] + 0.5) * gt[1], gt[3] + (cen[2 * k] + 0.5) * gt[5]);
            assert!((x - cx).abs() < 1e-6 && (y - cy).abs() < 1e-6, "{name}: segment {k}'s centre");
            assert_eq!(m.len(), f_n, "{name}: the means' columns");
            for (j, v) in m.iter().enumerate() {
                assert_eq!(v.to_bits(), means[k * f_n + j].to_bits(), "{name}: segment {k}, feature {j}");
            }
        }
        println!("{name}: {non_empty} segments as zeit's");
    }
}
