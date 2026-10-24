//! Post-processing of classes (a tool after the bridge): a raster run gives Zeit's desawtooth_classes, majority
//! filter and sieve of the class series exactly (tests/data/postproc/postproc.json, written by
//! tools/postproc_fixtures.py), with the layer's class names and colours on the cleaned series; a pixel run its
//! temporal filter.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use gdal::Dataset;
use janus_zeit::job::{ClassDef, JobSpec, run_job};
use janus_zeit::python::calls;
use janus_zeit::{Params, PixelInput, run_pixel};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
struct Case {
    params: BTreeMap<String, Value>,
    classes: Vec<Option<f64>>,
    changed: Vec<Option<f64>>,
}

#[derive(Deserialize)]
struct Fixtures {
    input: String,
    nodata: f64,
    years: Vec<f64>,
    days: Vec<i64>,
    cases: Vec<Case>,
}

/// Every band of a raster, one after the other, NoData as NaN.
fn read_all(path: &str) -> Vec<f64> {
    let ds = Dataset::open(path).unwrap();
    let (w, h) = ds.raster_size();
    let mut out = vec![];
    for b in 1..=ds.raster_count() {
        let band = ds.rasterband(b).unwrap();
        let nd = band.no_data_value();
        let data = band.read_as::<f64>((0, 0), (w, h), (w, h), None).unwrap();
        out.extend(data.data().iter().map(|&v| if nd.is_some_and(|d| d == v) || v.is_nan() { f64::NAN } else { v }));
    }
    out
}

fn same(got: &[f64], want: &[Option<f64>], what: &str) {
    assert_eq!(got.len(), want.len(), "{what}");
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        match w {
            Some(w) => assert_eq!(g, w, "{what} value {i}"),
            None => assert!(g.is_nan(), "{what} value {i}: {g}"),
        }
    }
}

#[test]
fn raster_run_is_zeits_post_processing() {
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let fx: Fixtures =
        serde_json::from_str(&std::fs::read_to_string(data.join("postproc/postproc.json")).unwrap()).unwrap();
    let classes = vec![
        ClassDef { value: 1, name: "Forest".into(), color: [20, 120, 40] },
        ClassDef { value: 2, name: "Savanna".into(), color: [160, 200, 60] },
        ClassDef { value: 3, name: "Pasture".into(), color: [230, 200, 120] },
        ClassDef { value: 4, name: "Crop".into(), color: [200, 80, 200] },
    ];
    for (k, c) in fx.cases.iter().enumerate() {
        let out = std::env::temp_dir().join(format!("janus-zeit-postproc-test-{k}"));
        let _ = std::fs::remove_dir_all(&out);
        let spec = JobSpec {
            tool: "postproc".into(),
            params: c.params.clone(),
            input: data.join(&fx.input).to_string_lossy().to_string(),
            nodata: Some(fx.nodata),
            years: fx.years.clone(),
            days: Some(fx.days.clone()),
            output_dir: out.to_string_lossy().to_string(),
            classes: classes.clone(),
            ..Default::default()
        };
        let result = run_job(&spec, &|_, _| {}, &AtomicBool::new(false)).unwrap();
        let path = |id: &str| {
            result.outputs.iter().find(|o| o["id"] == id).unwrap_or_else(|| panic!("no output {id}"))["path"]
                .as_str()
                .unwrap()
                .to_string()
        };
        same(&read_all(&path("classes")), &c.classes, &format!("case {k} classes"));
        same(&read_all(&path("changed")), &c.changed, &format!("case {k} changed"));
        // the cleaned series: one band of classes a date, with the layer's names and colours
        let ds = Dataset::open(path("classes")).unwrap();
        assert_eq!(ds.raster_count(), fx.years.len());
        let band = ds.rasterband(2).unwrap();
        let rat = unsafe {
            let h = band.c_rasterband();
            let rat = gdal_sys::GDALGetDefaultRAT(h);
            assert!(!rat.is_null(), "an attribute table");
            let row = |r: i32| {
                let name = std::ffi::CStr::from_ptr(gdal_sys::GDALRATGetValueAsString(rat, r, 1))
                    .to_string_lossy()
                    .into_owned();
                (gdal_sys::GDALRATGetValueAsInt(rat, r, 0), name, gdal_sys::GDALRATGetValueAsInt(rat, r, 2))
            };
            (gdal_sys::GDALRATGetRowCount(rat), row(2))
        };
        assert_eq!(rat, (4, (3, "Pasture".to_string(), 230)));
    }
}

#[test]
fn a_float_layer_is_refused() {
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/raster/ccdc-0");
    let mut spec: Value = serde_json::from_str(&std::fs::read_to_string(data.join("spec.json")).unwrap()).unwrap();
    for key in ["bands", "qa_input", "qa_rule", "shown"] {
        spec.as_object_mut().unwrap().remove(key);
    }
    let mut spec: JobSpec = serde_json::from_value(spec).unwrap();
    spec.tool = "postproc".into();
    spec.input = data.join(&spec.input).to_string_lossy().to_string();
    let out = std::env::temp_dir().join("janus-zeit-postproc-test-float");
    spec.output_dir = out.to_string_lossy().to_string();
    let err = run_job(&spec, &|_, _| {}, &AtomicBool::new(false)).err().expect("an error");
    assert!(err.contains("not a class"), "{err}");
}

#[test]
fn pixel_run_removes_excursions() {
    let values = vec![1.0, 1.0, 3.0, 1.0, f64::NAN, 2.0, 2.0, 1.0, 2.0];
    let input = PixelInput {
        years: (0..values.len()).map(|i| 2015.5 + i as f64).collect(),
        values: values.clone(),
        ..Default::default()
    };
    let p = Params::with_defaults("postproc", &BTreeMap::new()).unwrap();
    let r = run_pixel("postproc", &p, &input).unwrap();
    let row = |label: &str| r.rows.iter().find(|x| x[0] == label).map(|x| x[1].clone()).unwrap();
    // the 3 between 1s and the 1 between 2s
    assert_eq!(row("Dates changed"), "2");
    assert!(row("Spatial filters").contains("majority 3 x 3") && row("Spatial filters").contains("6 px"));
    let line = r.overlays.iter().find(|o| o.label == "Classes after the filter").unwrap();
    let ys: Vec<Option<f64>> = line.y.clone();
    assert_eq!(ys, vec![Some(1.0), Some(1.0), Some(1.0), Some(1.0), None, Some(2.0), Some(2.0), Some(2.0), Some(2.0)]);
    // off: nothing changes
    let p = Params::with_defaults("postproc", &BTreeMap::from([("max_length".into(), json!(0))])).unwrap();
    let r = run_pixel("postproc", &p, &input).unwrap();
    assert!(r.overlays.is_empty());
    // Copy Python: the same calls
    let p = Params::with_defaults("postproc", &BTreeMap::new()).unwrap();
    let (code, needs) = calls("postproc", &p, &input).unwrap();
    assert_eq!(needs, vec!["series"]);
    assert!(code.contains("zeit.desawtooth_classes(series, max_length=1)"), "{code}");
    assert!(
        code.contains("zeit.mmu_sieve(zeit.apply_majority_filter(m, 3, iterations=1), min_size=6, connectivity=8)"),
        "{code}"
    );
}
