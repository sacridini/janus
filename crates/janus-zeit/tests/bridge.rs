//! The native tools against the Python bridge on Zeit 2.0 (tests/data, written by tools/zeit_fixtures.py): the
//! same overlays, bit for bit, and the same rows.

use std::collections::BTreeMap;

use janus_zeit::{Manifest, Params, PixelInput, PixelResult, manifest, run_pixel};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
struct Case {
    params: BTreeMap<String, Value>,
    years: Vec<f64>,
    days: Option<Vec<i64>>,
    values: Vec<Option<f64>>,
    result: Value,
}

#[derive(Deserialize)]
struct Fixtures {
    cases: Vec<Case>,
}

fn data(name: &str) -> String {
    let path = format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

#[test]
fn descriptors_are_the_bridges() {
    let bridge: Manifest = serde_json::from_str(&data("tools.json")).unwrap();
    assert_eq!(manifest().tools.len(), bridge.tools.len());
    for (a, b) in manifest().tools.iter().zip(&bridge.tools) {
        assert_eq!(a, b, "{}", a.id);
    }
}

/// Every case of a tool's fixtures; the first differences are printed, then the test fails.
fn compare(tool: &str) {
    let f: Fixtures = serde_json::from_str(&data(&format!("{tool}.json"))).unwrap();
    let mut bad = 0;
    for (k, c) in f.cases.iter().enumerate() {
        let p = Params::with_defaults(tool, &c.params).unwrap();
        let input = PixelInput {
            years: c.years.clone(),
            days: c.days.clone(),
            values: c.values.iter().map(|v| v.unwrap_or(f64::NAN)).collect(),
        };
        let got = run_pixel(tool, &p, &input);
        let ok = match (&got, c.result.get("error")) {
            (Err(_), Some(_)) => true,
            (Ok(r), None) => {
                let want: PixelResult = serde_json::from_value(c.result.clone()).unwrap();
                *r == want
            }
            _ => false,
        };
        if !ok {
            bad += 1;
            if bad <= 3 {
                println!("{tool} case {k} ({:?}):\n  native {got:?}\n  bridge {}", c.params, c.result);
            }
        }
    }
    assert_eq!(bad, 0, "{tool}: {bad} of {} cases differ from the bridge", f.cases.len());
    println!("{tool}: {} cases as the bridge", f.cases.len());
}

#[test]
fn landtrendr() {
    compare("landtrendr");
}
