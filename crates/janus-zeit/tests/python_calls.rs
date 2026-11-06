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
