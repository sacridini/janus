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

/// Every case of a tool's fixtures, the same bits; the first differences are printed, then the test fails.
fn compare(tool: &str) {
    compare_within(tool, |_| 0.0);
}

/// Overlay values within `tol(case)` relative to the bridge's (for what Zeit leaves to scipy's LAPACK).
fn close(a: &PixelResult, b: &PixelResult, tol: f64) -> bool {
    let near = |x: f64, y: f64| x == y || (x - y).abs() <= tol * x.abs().max(y.abs()).max(1e-300);
    // a row's number printed to 4-5 digits may round the other way, a zero's noise differs
    let num = |t: &str| t.split_whitespace().next().and_then(|w| w.parse::<f64>().ok());
    let row_near = |p: &[String; 2], q: &[String; 2]| {
        p == q
            || (tol > 0.0
                && p[0] == q[0]
                && match (num(&p[1]), num(&q[1])) {
                    (Some(x), Some(y)) => (x - y).abs() <= 1e-12 || (x - y).abs() <= 2e-4 * x.abs().max(y.abs()),
                    _ => false,
                })
    };
    a.rows.len() == b.rows.len()
        && a.rows.iter().zip(&b.rows).all(|(p, q)| row_near(p, q))
        && a.overlays.len() == b.overlays.len()
        && a.overlays.iter().zip(&b.overlays).all(|(p, q)| {
            p.kind == q.kind
                && p.label == q.label
                && p.x.len() == q.x.len()
                && p.y.len() == q.y.len()
                && p.x.iter().zip(&q.x).all(|(&x, &y)| near(x, y))
                && p.y.iter().zip(&q.y).all(|(x, y)| match (x, y) {
                    (Some(x), Some(y)) => near(*x, *y),
                    (None, None) => true,
                    _ => false,
                })
        })
}

fn compare_within(tool: &str, tol: impl Fn(&Case) -> f64) {
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
                close(r, &want, tol(c))
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

#[test]
fn mann_kendall() {
    compare("mann_kendall");
}

#[test]
fn smooth() {
    // Savitzky-Golay: scipy's polynomials, to its rounding (LAPACK on an unscaled Vandermonde: ~1e-8 at a window
    // of 61 and order 6); Whittaker: the same bits
    compare_within("smooth", |c| if c.params.get("method").is_some_and(|m| m == "savgol") { 1e-7 } else { 0.0 });
}
