//! "Copy Python" of the FORCE time-series tools: each has an entry in `calls`, naming the Zeit function and
//! passing the tool's parameters (as tools/tsa_fixtures.py calls them).

use std::collections::BTreeMap;

use janus_zeit::python::calls;
use janus_zeit::{Params, PixelInput};
use serde_json::{Value, json};

fn code(tool: &str, given: &[(&str, Value)]) -> String {
    let given: BTreeMap<String, Value> = given.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
    let p = Params::with_defaults(tool, &given).unwrap();
    let input = PixelInput {
        years: vec![2020.0, 2020.5],
        days: Some(vec![18300, 18400]),
        values: vec![0.1, 0.2],
        ..Default::default()
    };
    let (code, inputs) = calls(tool, &p, &input).unwrap_or_else(|| panic!("{tool}: no Python calls"));
    assert_eq!(inputs, vec!["series"], "{tool}");
    code
}

#[test]
fn the_time_series_tools_have_python_calls() {
    let c = code("interpolate", &[("method", json!("moving")), ("step", json!(9)), ("window", json!(7.0))]);
    assert!(c.contains("zeit.interpolate(series, 'moving', step=9, window=7.0"), "{c}");
    let c = code("interpolate", &[("method", json!("rbf")), ("sigma", json!("4,8,16")), ("cutoff", json!(0.9))]);
    assert!(c.contains("sigma=[4.0, 8.0, 16.0], cutoff=0.9"), "{c}");
    let c = code("interpolate", &[("method", json!("harmonic")), ("modes", json!(2)), ("trend", json!(false))]);
    assert!(c.contains("modes=2, trend=False"), "{c}");

    let c = code("temporal_metrics", &[("metrics", json!(["median", "p10"]))]);
    assert!(c.contains("zeit.temporal_metrics(series, ['median', 'p10']"), "{c}");
    let c = code("fold", &[("metrics", json!(["mean"])), ("by", json!("month"))]);
    assert!(c.contains("zeit.temporal_metrics(series, ['mean'], by='month'"), "{c}");
    let c = code("standardize", &[("method", json!("center"))]);
    assert!(c.contains("zeit.standardize(series, 'center'"), "{c}");

    let c = code("linear_trend", &[("by", json!("quarter")), ("conf", json!(0.9)), ("tail", json!("left"))]);
    assert!(
        c.contains("by='quarter'") && c.contains("zeit.linear_trend(result1, dim='quarter', conf=0.9, tail='left'"),
        "{c}"
    );
    let c = code("cat", &[("by", json!("year")), ("penalty", json!(true))]);
    assert!(c.contains("zeit.cat(result1, penalty=True, dim='year'"), "{c}");

    let c = code("clear_observations", &[("months", json!(3)), ("statistics", json!(["count", "max"]))]);
    assert!(c.contains("zeit.clear_observations(series, months=3, metrics=['count', 'max']"), "{c}");
    let c =
        code("polar_metrics", &[("metrics", json!(["VSS", "IGS"])), ("adaptive", json!(false)), ("mid", json!(0.4))]);
    assert!(
        c.contains("zeit.polar_metrics(series, ['VSS', 'IGS'], start=0.2, mid=0.4, end=0.8, adaptive=False"),
        "{c}"
    );
}

#[test]
fn eof_and_lagged_correlation_have_python_calls() {
    let c = code(
        "eof",
        &[("n_modes", json!(5)), ("rotate", json!(3)), ("north_n", json!("dates")), ("weights", json!("none"))],
    );
    assert!(c.contains("zeit.eof(series, n_modes=5, anomaly='month', harmonics=2, weights='none'"), "{c}");
    assert!(c.contains("rotate=3, kaiser=True, north_n='dates', correlation_maps=False, seed=42"), "{c}");
    // the compact call leaves out what Zeit's default is: of the defaults, only North's n differs
    let input = PixelInput { years: vec![2020.0], days: Some(vec![18300]), values: vec![0.1], ..Default::default() };
    let p = Params::with_defaults("eof", &BTreeMap::new()).unwrap();
    let call = janus_zeit::python::tool_call("eof", &p, &input).unwrap();
    assert_eq!(call.compact("").0, "result = zeit.eof(series, north_n='autocorrelation')");

    // the driver: another series (its own cube), its mean over the window, or a CSV
    let given = |v: &[(&str, Value)]| -> Params {
        Params::with_defaults("lagged_correlation", &v.iter().map(|(k, x)| (k.to_string(), x.clone())).collect())
            .unwrap()
    };
    let (c, inputs) =
        calls("lagged_correlation", &given(&[("lags", json!(6)), ("method", json!("spearman"))]), &input).unwrap();
    assert_eq!(inputs, vec!["series", "other_cube"]);
    assert!(
        c.contains("zeit.lagged_correlation(series, other_cube, lags=6, step='month', match='nearest', tolerance='8D'"),
        "{c}"
    );
    assert!(
        c.contains("method='spearman', best='absolute', min_pairs=10, effective_n=True, sidak=True, alpha_fdr=0.1"),
        "{c}"
    );
    let (c, inputs) = calls("lagged_correlation", &given(&[("y_mode", json!("window_mean"))]), &input).unwrap();
    assert_eq!(inputs, vec!["series", "other_cube"]);
    assert!(c.contains("zeit.lagged_correlation(series, other_cube.mean(dim=('y', 'x')), lags=3"), "{c}");
    let (c, inputs) = calls(
        "lagged_correlation",
        &given(&[("y_mode", json!("file")), ("y_file", json!("oni.csv")), ("tolerance", json!(12.5))]),
        &input,
    )
    .unwrap();
    assert_eq!(inputs, vec!["series"]);
    assert!(c.starts_with("driver = pd.read_csv('oni.csv', index_col=0, parse_dates=True).iloc[:, 0]"), "{c}");
    assert!(
        c.contains("zeit.lagged_correlation(series, driver, lags=3, step='month', match='nearest', tolerance='12.5D'"),
        "{c}"
    );
    // the defaults leave out of the compact call everything but the driver
    let call = janus_zeit::python::tool_call("lagged_correlation", &given(&[]), &input).unwrap();
    assert_eq!(call.compact("").0, "result = zeit.lagged_correlation(series, other_cube)");
}

#[test]
fn mnf_cva_imad_and_radcal_have_python_calls() {
    let given = |tool: &str, v: &[(&str, Value)]| -> Params {
        Params::with_defaults(tool, &v.iter().map(|(k, x)| (k.to_string(), x.clone())).collect()).unwrap()
    };
    let input = PixelInput {
        years: vec![2019.0, 2020.0, 2021.0],
        days: Some(vec![17900, 18300, 18700]),
        values: vec![0.1, 0.2, 0.3],
        band_numbers: [("red", 3), ("nir", 4), ("swir1", 5)].iter().map(|(r, b)| (r.to_string(), *b)).collect(),
        ..Default::default()
    };
    // MNF of a date: the components, then both
    let (c, inputs) =
        calls("mnf", &given("mnf", &[("date", json!(2020.2)), ("n_components", json!(4))]), &input).unwrap();
    assert_eq!(inputs, vec!["bands_all"]);
    assert!(c.starts_with("result1 = zeit.mnf(bands_all.isel(time=1), 4, noise='diff_hv', scale='noise'"), "{c}");
    let (c, _) = calls(
        "mnf",
        &given(
            "mnf",
            &[("scope", json!("all_dates")), ("output", json!("both")), ("k", json!(3)), ("bands", json!("1, 2,nir"))],
        ),
        &input,
    )
    .unwrap();
    assert!(
        c.starts_with("result1, transform = zeit.mnf(bands_all, None, noise='diff_hv', bands=['b001', 'b002', 'nir']"),
        "{c}"
    );
    assert!(
        c.contains("return_transform=True")
            && c.contains("result2 = zeit.mnf_denoise(bands_all, k=3, transform=transform"),
        "{c}"
    );
    let (c, _) = calls("mnf", &given("mnf", &[("output", json!("denoised"))]), &input).unwrap();
    assert!(c.contains("zeit.mnf_denoise(bands_all.isel(time=2), snr_min=1.0, noise='diff_hv'"), "{c}");
    // CVA: the two dates by index, the sign sectors with the bands' names in bands_all
    let (c, _) = calls("cva", &given("cva", &[("threshold", json!("em"))]), &input).unwrap();
    assert!(
        c.contains(
            "zeit.cva(bands_all, dates=(0, 2), roles={'red': 'b003', 'nir': 'b004', 'swir1': 'b005'}, direction='sign'"
        ),
        "{c}"
    );
    assert!(c.contains("threshold='em', em_init=0.2, sample=1000000"), "{c}");
    let (c, _) = calls(
        "cva",
        &given(
            "cva",
            &[
                ("direction", json!("c2va")),
                ("threshold", json!("fixed")),
                ("value", json!(0.25)),
                ("sample", json!(0)),
            ],
        ),
        &input,
    )
    .unwrap();
    assert!(c.contains("direction='c2va', scale='none', threshold=0.25") && c.contains("sample=None"), "{c}");
    // iMAD and the normalization
    let (c, _) = calls(
        "imad",
        &given("imad", &[("before", json!(2019.9)), ("after", json!(2021.0)), ("lam", json!(0.5))]),
        &input,
    )
    .unwrap();
    assert!(c.contains("zeit.imad(bands_all, dates=(1, 2), max_iter=50, tol=0.001, lam=0.5, penalty='ridge'"), "{c}");
    let (c, _) = calls("radcal", &given("radcal", &[("target", json!(2021.0))]), &input).unwrap();
    assert!(
        c.contains("zeit.radcal(bands_all, reference=0, targets=[2], threshold=0.95, holdout=0.3333333333333333"),
        "{c}"
    );
    // the compact call leaves out Zeit's defaults
    let call = janus_zeit::python::tool_call("imad", &given("imad", &[]), &input).unwrap();
    assert_eq!(call.compact("").0, "result = zeit.imad(bands_all, dates=(0, 2))");
    let call = janus_zeit::python::tool_call("radcal", &given("radcal", &[]), &input).unwrap();
    assert_eq!(call.compact("").0, "result = zeit.radcal(bands_all, reference=0)");
}
