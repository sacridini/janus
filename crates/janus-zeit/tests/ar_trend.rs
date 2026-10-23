//! AR(1) trend (a tool after the bridge): a raster run gives zeit-core's fit of every pixel (zeit-core's own tests
//! check that fit against R's remotePARTS), and a pixel run its rows and line.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use gdal::Dataset;
use janus_zeit::job::{JobSpec, run_job};
use janus_zeit::{Params, PixelInput, run_pixel};
use serde_json::{Value, json};
use zeit_core::ar_reml::{self, Gaps};

/// A raster's bands, pixel by pixel ([pixel][band]), NoData as NaN.
#[allow(clippy::needless_range_loop)]
fn read_stack(path: &Path) -> (usize, usize, Vec<Vec<f64>>) {
    let ds = Dataset::open(path).unwrap();
    let (w, h) = ds.raster_size();
    let n = ds.raster_count();
    let mut out = vec![vec![f64::NAN; n]; w * h];
    for b in 0..n {
        let band = ds.rasterband(b + 1).unwrap();
        let nd = band.no_data_value();
        let data = band.read_as::<f64>((0, 0), (w, h), (w, h), None).unwrap();
        for (i, &v) in data.data().iter().enumerate() {
            out[i][b] = if nd.is_some_and(|d| d == v) { f64::NAN } else { v };
        }
    }
    (w, h, out)
}

#[test]
fn raster_run_is_zeit_cores_fit() {
    let case = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/raster/mann_kendall-0");
    let mut spec: JobSpec = serde_json::from_str(&std::fs::read_to_string(case.join("spec.json")).unwrap()).unwrap();
    spec.tool = "ar_trend".into();
    spec.params = BTreeMap::from([("harmonics".to_string(), json!(0)), ("time".to_string(), json!("year"))]);
    spec.input = case.join(&spec.input).to_string_lossy().to_string();
    let out = std::env::temp_dir().join("janus-zeit-ar-trend-test");
    let _ = std::fs::remove_dir_all(&out);
    spec.output_dir = out.to_string_lossy().to_string();
    let result = run_job(&spec, &|_, _| {}, &AtomicBool::new(false)).unwrap();
    let path = |id: &str| {
        let o = result.outputs.iter().find(|o| o["id"] == id).unwrap_or_else(|| panic!("no output {id}"));
        assert_eq!(o["unit"].as_str(), if id == "slope" { Some("value/year") } else { o["unit"].as_str() });
        o["path"].as_str().unwrap().to_string()
    };
    let (w, h, stack) = read_stack(Path::new(&spec.input));
    let (_, _, slope) = read_stack(Path::new(&path("slope")));
    let (_, _, p) = read_stack(Path::new(&path("p")));
    let (_, _, sig) = read_stack(Path::new(&path("sig_slope")));
    let n = spec.years.len();
    let mut x = vec![1.0; n];
    x.extend_from_slice(&spec.years);
    let mut fitted = 0;
    for i in 0..w * h {
        match ar_reml::fit(&stack[i], &x, 2, Gaps::Exact) {
            Some(f) if stack[i].iter().filter(|v| v.is_finite()).count() >= 4 => {
                // the maps are float32
                assert_eq!(slope[i][0], f.coefficients[1] as f32 as f64, "pixel {i}");
                assert_eq!(p[i][0], f.p[1] as f32 as f64, "pixel {i}");
                // the significant slope where p < 0.05, NaN elsewhere
                assert!(sig[i][0].is_nan() != (f.p[1] < 0.05), "pixel {i}");
                fitted += 1;
            }
            _ => assert!(slope[i][0].is_nan(), "pixel {i}"),
        }
    }
    assert!(fitted > w * h / 2, "{fitted} of {} pixels fitted", w * h);
}

#[test]
fn pixel_run_rows_and_line() {
    // monthly, a trend and a yearly cycle
    let years: Vec<f64> = (0..48).map(|i| 2000.0 + (i as f64 + 0.5) / 12.0).collect();
    let values: Vec<f64> = (0..48)
        .map(|i| {
            let y = years[i];
            let v =
                0.5 + 0.01 * (y - 2000.0) + 0.1 * (2.0 * std::f64::consts::PI * y).cos() + 0.003 * ((i * 7) % 5) as f64;
            if i == 7 { f64::NAN } else { v }
        })
        .collect();
    let input = PixelInput { years: years.clone(), values: values.clone(), ..Default::default() };
    let p = Params::with_defaults("ar_trend", &BTreeMap::from([("harmonics".to_string(), Value::from(1))])).unwrap();
    let r = run_pixel("ar_trend", &p, &input).unwrap();
    let row = |label: &str| r.rows.iter().find(|row| row[0] == label).map(|row| row[1].clone()).unwrap();
    assert_eq!(row("Trend"), "increasing");
    assert!(row("Slope").ends_with("/yr"), "{}", row("Slope"));
    assert_eq!(r.overlays.len(), 1);
    assert_eq!(r.overlays[0].x, years);
    // harmonics of annual dates: constant, not a model
    let annual: Vec<f64> = (0..20).map(|i| 2000.5 + i as f64).collect();
    let r =
        run_pixel("ar_trend", &p, &PixelInput { years: annual, values: values[..20].to_vec(), ..Default::default() })
            .unwrap();
    assert!(r.overlays.is_empty() && r.rows[0][1].contains("sub-annual"), "{:?}", r.rows);
    // too few values
    let short = PixelInput { years: years[..3].to_vec(), values: vec![1.0, 2.0, 3.0], ..Default::default() };
    let r = run_pixel("ar_trend", &p, &short).unwrap();
    assert!(r.overlays.is_empty() && r.rows[0][1].starts_with("not enough data"));
}
