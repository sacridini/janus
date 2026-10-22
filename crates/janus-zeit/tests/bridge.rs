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
    #[serde(default)]
    bands: BTreeMap<String, Vec<Option<f64>>>,
    #[serde(default)]
    qa: Option<Vec<Option<f64>>>,
    #[serde(default)]
    qa_rule: Option<String>,
    #[serde(default)]
    shown: Option<janus_zeit::pixel::Shown>,
    #[serde(default)]
    map_year: Option<f64>,
    result: Value,
}

#[derive(Deserialize)]
struct Fixtures {
    cases: Vec<Case>,
}

/// JSON's nulls as NaN.
fn nan(v: &[Option<f64>]) -> Vec<f64> {
    v.iter().map(|x| x.unwrap_or(f64::NAN)).collect()
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
    compare_within(tool, |_| Tol::default());
}

/// What may differ from the bridge, for what Zeit leaves to numpy's or scipy's LAPACK: the curves' values,
/// relative to each curve's largest; the numbers the rows print (to their last digit).
#[derive(Clone, Copy, Default)]
struct Tol {
    curves: f64,
    rows: bool,
}

fn close(a: &PixelResult, b: &PixelResult, tol: Tol) -> bool {
    // a row's number printed to 4-5 digits may round the other way, a zero's noise differs
    let num = |t: &str| t.split_whitespace().next().and_then(|w| w.parse::<f64>().ok());
    let row_near = |p: &[String; 2], q: &[String; 2]| {
        p == q
            || (tol.rows
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
            let scale = q.y.iter().flatten().fold(0.0f64, |m, v| m.max(v.abs())).max(1e-300);
            let near = |x: f64, y: f64| x == y || (x - y).abs() <= tol.curves * scale;
            p.kind == q.kind
                && p.label == q.label
                && p.x == q.x
                && p.y.len() == q.y.len()
                && p.y.iter().zip(&q.y).all(|(x, y)| match (x, y) {
                    (Some(x), Some(y)) => near(*x, *y),
                    (None, None) => true,
                    _ => false,
                })
        })
}

/// Where a native run and the bridge's answer part.
fn first_difference(got: &Result<PixelResult, String>, want: &Value) -> String {
    let (r, w) = match (got, serde_json::from_value::<PixelResult>(want.clone())) {
        (Ok(r), Ok(w)) => (r, w),
        (got, _) => return format!("native {got:?}, bridge {want}"),
    };
    if r.rows != w.rows {
        return format!("rows\n  native {:?}\n  bridge {:?}", r.rows, w.rows);
    }
    let labels = |o: &[janus_zeit::Overlay]| o.iter().map(|o| o.label.clone()).collect::<Vec<_>>();
    if r.overlays.len() != w.overlays.len() {
        return format!("overlays {:?} vs {:?}", labels(&r.overlays), labels(&w.overlays));
    }
    // the overlay of the largest difference, relative to its curve's largest value
    let mut worst = (0.0f64, String::new());
    for (a, b) in r.overlays.iter().zip(&w.overlays) {
        if a.label != b.label || a.x.len() != b.x.len() || a.y.len() != b.y.len() {
            return format!("{} ({}, {}) vs {} ({}, {})", a.label, a.x.len(), a.y.len(), b.label, b.x.len(), b.y.len());
        }
        if a.x != b.x {
            return format!("{}: x differs", a.label);
        }
        let scale = b.y.iter().flatten().fold(0.0f64, |m, v| m.max(v.abs())).max(1e-300);
        for i in 0..a.y.len() {
            match (a.y[i], b.y[i]) {
                (Some(p), Some(q)) if (p - q).abs() / scale > worst.0 => {
                    worst = ((p - q).abs() / scale, format!("{} [{i}]: {p} vs {q}", a.label));
                }
                (p, q) if p.is_some() != q.is_some() => return format!("{} [{i}]: {p:?} vs {q:?}", a.label),
                _ => {}
            }
        }
    }
    if worst.0 > 0.0 {
        return format!("{} (relative to the curve {:.2e})", worst.1, worst.0);
    }
    "the same".into()
}

/// Whether the fixtures' values are compared: their bits are those of the machine that wrote them (its C library's
/// exp, sin...), which an iterative fit follows to its end; on CI's runners (CI set) only that every case runs, or
/// fails, as the bridge's did, and how many give the same answer is printed.
fn values_compared() -> bool {
    std::env::var_os("CI").is_none()
}

fn compare_within(tool: &str, tol: impl Fn(&Case) -> Tol) {
    let f: Fixtures = serde_json::from_str(&data(&format!("{tool}.json"))).unwrap();
    let (mut bad, mut same) = (0, 0);
    for (k, c) in f.cases.iter().enumerate() {
        let p = Params::with_defaults(tool, &c.params).unwrap();
        let input = PixelInput {
            years: c.years.clone(),
            days: c.days.clone(),
            values: nan(&c.values),
            bands: c.bands.iter().map(|(r, v)| (r.clone(), nan(v))).collect(),
            qa: c.qa.as_deref().map(nan),
            qa_rule: c.qa_rule.clone(),
            shown: c.shown.clone(),
            map_year: c.map_year,
        };
        let got = run_pixel(tool, &p, &input);
        let ok = match (&got, c.result.get("error")) {
            (Err(_), Some(_)) => true,
            (Ok(r), None) => {
                let want: PixelResult = serde_json::from_value(c.result.clone()).unwrap();
                let near = close(r, &want, tol(c));
                same += usize::from(near);
                near || !values_compared()
            }
            _ => false,
        };
        if !ok {
            bad += 1;
            if bad <= 3 {
                println!("{tool} case {k} ({:?}): {}", c.params, first_difference(&got, &c.result));
            }
        }
    }
    assert_eq!(bad, 0, "{tool}: {bad} of {} cases differ from the bridge", f.cases.len());
    println!("{tool}: {} cases run as the bridge's, {same} with its answer", f.cases.len());
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
    compare_within("smooth", |c| {
        let savgol = c.params.get("method").is_some_and(|m| m == "savgol");
        Tol { curves: if savgol { 1e-7 } else { 0.0 }, rows: savgol }
    });
}

// BFAST: the breaks and rows the same bits; the models drawn are refitted by least squares (numpy's LAPACK in
// the bridge), to rounding
#[test]
fn bfast() {
    compare_within("bfast", |_| Tol { curves: 1e-8, rows: false });
}

#[test]
fn bfast_lite() {
    compare_within("bfast_lite", |_| Tol { curves: 1e-8, rows: false });
}

#[test]
fn bfast_monitor() {
    compare_within("bfast_monitor", |_| Tol { curves: 1e-8, rows: false });
}

#[test]
fn tmask() {
    compare("tmask");
}

#[test]
fn ndfi() {
    compare("ndfi");
}

#[test]
fn coded() {
    compare("coded");
}

/// Python's timedelta(days=x).days, the bridge's phenology dates.
#[test]
fn timedelta_days() {
    let mut bad = 0;
    for line in data("timedelta.txt").lines() {
        let (x, d) = line.split_once(' ').unwrap();
        let (x, d): (f64, i64) = (x.parse().unwrap(), d.parse().unwrap());
        if janus_zeit::numeric::timedelta_days(x) != d {
            bad += 1;
            println!("timedelta(days={x:?}).days = {d}, native {}", janus_zeit::numeric::timedelta_days(x));
        }
    }
    assert_eq!(bad, 0);
}

#[test]
fn phenology() {
    compare("phenology");
}

#[test]
fn twdtw() {
    compare("twdtw");
}

// CCDC: the segments, dates, magnitudes and rows the same bits; the models drawn go through numpy's cos/sin and
// BLAS (dgemv): to rounding
#[test]
fn ccdc() {
    compare_within("ccdc", |_| Tol { curves: 1e-12, rows: false });
}

#[test]
fn ccdc_synthetic() {
    compare_within("ccdc_synthetic", |_| Tol { curves: 1e-12, rows: false });
}

/// "Copy Python": the first Zeit call of each fixture case as the bridge recorded it.
#[test]
fn python_calls_as_the_bridge_recorded() {
    let expected: Vec<Value> = serde_json::from_str(&data("python_calls.json")).unwrap();
    let mut bad = 0;
    for e in &expected {
        let tool = e["tool"].as_str().unwrap();
        let f: Fixtures = serde_json::from_str(&data(&format!("{tool}.json"))).unwrap();
        let c = &f.cases[e["case"].as_u64().unwrap() as usize];
        let p = Params::with_defaults(tool, &c.params).unwrap();
        let input = PixelInput {
            years: c.years.clone(),
            days: c.days.clone(),
            values: nan(&c.values),
            bands: c.bands.iter().map(|(r, v)| (r.clone(), nan(v))).collect(),
            ..Default::default()
        };
        let (code, _) = janus_zeit::python::calls(tool, &p, &input).unwrap();
        let first = code.lines().next().unwrap_or("");
        if first != e["first_call"].as_str().unwrap() {
            bad += 1;
            println!("{tool}:\n  native {first}\n  bridge {}", e["first_call"]);
        }
    }
    assert_eq!(bad, 0, "{bad} of {} calls differ", expected.len());
}
