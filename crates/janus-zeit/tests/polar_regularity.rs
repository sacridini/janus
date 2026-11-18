//! polar_metrics on the series a user opens: a regular step runs (with Zeit's values), anything else says why in one
//! clear sentence.

use std::collections::BTreeMap;

use janus_zeit::{Params, PixelInput, run_pixel};

/// Days since 1970-01-01 of a civil date (Hinnant's algorithm).
fn days(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// A seasonal index peaking in mid-year, on the dates `ds` (the same formula as the Zeit Python reference).
fn series(ds: &[i64]) -> PixelInput {
    let values = ds
        .iter()
        .map(|&d| {
            let doy = d.rem_euclid(365) as f64;
            0.5 + 0.4 * (2.0 * std::f64::consts::PI * (doy - 100.0) / 365.25).sin()
        })
        .collect();
    PixelInput { days: Some(ds.to_vec()), values, ..Default::default() }
}

fn run(input: &PixelInput) -> Result<Vec<[String; 2]>, String> {
    let params = Params::with_defaults("polar_metrics", &BTreeMap::new()).unwrap();
    run_pixel("polar_metrics", &params, input).map(|r| r.rows)
}

fn regular(start: i64, step: i64, n: i64) -> Vec<i64> {
    (0..n).map(|i| start + step * i).collect()
}

/// The value of a row ("VSS, 2019") of a run, None when the year has no value.
fn value(rows: &[[String; 2]], label: &str) -> Option<f64> {
    rows.iter().find(|r| r[0] == label).and_then(|r| r[1].parse().ok())
}

/// zeit.polar_metrics (Zeit's Python, the reference) on the same series, the six default metrics by year, rounded to
/// 6 decimals: the first year is empty.
const ZEIT_16_DAYS: [(&str, [f64; 4]); 6] = [
    ("VSS", [0.679178, 0.697387, 0.715071, 0.63492]),
    ("VPS", [0.899994, 0.899573, 0.898087, 0.896839]),
    ("VES", [0.683013, 0.664421, 0.645393, 0.625977]),
    ("VSA", [0.796255, 0.792542, 0.786719, 0.789783]),
    ("RMR", [0.194884, 0.191217, 0.18704, 0.201409]),
    ("IGS", [0.049378, 0.050048, 0.05067, 0.068406]),
];

/// The same for 10-day dates from 7 March (not a 1 January start), 2019 to 2021 (the last years are cut by the end of
/// the series).
const ZEIT_10_DAYS: [(&str, [f64; 3]); 6] = [
    ("VSS", [0.647793, 0.679178, 0.647793]),
    ("VPS", [0.899972, 0.8989, 0.899972]),
    ("VES", [0.638961, 0.67067, 0.638961]),
    ("VSA", [0.797606, 0.796534, 0.797606]),
    ("RMR", [0.197421, 0.191585, 0.197421]),
    ("IGS", [0.063769, 0.052105, 0.063769]),
];

#[test]
fn a_16_day_series_gives_zeits_values() {
    let rows = run(&series(&regular(days(2018, 1, 1), 16, 5 * 23))).unwrap();
    assert_eq!(rows[1][1], "16 days, 5 years");
    let mut worst = 0.0_f64;
    for (m, want) in ZEIT_16_DAYS {
        assert_eq!(value(&rows, &format!("{m}, 2018")), None, "the first year is empty");
        for (k, w) in want.iter().enumerate() {
            let got = value(&rows, &format!("{m}, {}", 2019 + k)).unwrap();
            worst = worst.max((got - w).abs());
        }
    }
    println!("max difference with Zeit Python (16 days): {worst:e}");
    assert!(worst < 2e-6, "{worst}");
}

#[test]
fn a_10_day_series_starting_in_march_gives_zeits_values() {
    let rows = run(&series(&regular(days(2018, 3, 7), 10, 180))).unwrap();
    let mut worst = 0.0_f64;
    for (m, want) in ZEIT_10_DAYS {
        for (k, w) in want.iter().enumerate() {
            let got = value(&rows, &format!("{m}, {}", 2019 + k)).unwrap();
            worst = worst.max((got - w).abs());
        }
    }
    println!("max difference with Zeit Python (10 days, March start): {worst:e}");
    assert!(worst < 2e-6, "{worst}");
}

#[test]
fn missing_steps_are_filled_and_counted() {
    let mut s = series(&regular(days(2018, 1, 1), 16, 5 * 23));
    let n = s.values.len();
    for i in (0..n).step_by(7) {
        s.values[i] = f64::NAN;
    }
    let rows = run(&s).unwrap();
    assert_eq!(rows[0][1], format!("{} of {n}", n - n.div_ceil(7)));
    let vps = value(&rows, "VPS, 2020").unwrap();
    assert!((vps - 0.899573).abs() < 1e-5, "{vps}");
}

#[test]
fn irregular_and_monthly_dates_are_refused_with_the_way_out() {
    // Landsat-like: a date every 16 days, shifted by a few
    let landsat: Vec<i64> = (0..80).map(|i| days(2018, 1, 5) + i * 16 + (i * 7) % 5).collect();
    let e = run(&series(&landsat)).unwrap_err();
    assert!(e.contains("not evenly spaced") && e.contains("Interpolate (regular grid)"), "{e}");
    assert!(!e.contains("28 to 31"), "{e}");
    // calendar months are not a constant number of days
    let months: Vec<i64> = (0..60).map(|i| days(2018 + i / 12, i % 12 + 1, 1)).collect();
    let e = run(&series(&months)).unwrap_err();
    assert!(e.contains("28 to 31 days") && e.contains("Interpolate (regular grid)"), "{e}");
}
