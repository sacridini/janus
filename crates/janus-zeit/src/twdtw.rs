//! TWDTW classification (zeit_bridge/tool_twdtw.py): the class of the reference series (patterns, from pins or
//! the ROI) most like the pixel's, by zeit.twdtw's Time-Weighted Dynamic Time Warping (zeit-core's engine; the
//! patterns' and dates' preparation of zeit/_twdtw_api.py here).

use serde_json::Value;
use zeit_core::twdtw::{Params as TwParams, Series, batch};

use crate::pixel::{Overlay, Params, PixelInput, PixelResult, g, row};

/// A class's reference series: its name, dates (days since 1970) and values sorted by date, and the decimal year
/// of each (NaN unknown), in the given order.
pub struct Pattern {
    pub name: String,
    pub days: Vec<i64>,
    pub values: Vec<f64>,
    pub years: Vec<f64>,
}

/// The "patterns" parameter: [{"name", "years", "days", "values"}], gaps dropped (the bridge's `_patterns`).
pub fn patterns(p: &Params) -> Result<Vec<Pattern>, String> {
    let list = p.0.get("patterns").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut out = vec![];
    for (k, pat) in list.iter().enumerate() {
        let name = pat
            .get("name")
            .and_then(Value::as_str)
            .filter(|n| !n.is_empty())
            .map_or_else(|| format!("Class {}", k + 1), str::to_string);
        let floats = |key: &str| -> Vec<f64> {
            pat.get(key)
                .and_then(Value::as_array)
                .map(|a| a.iter().map(|v| v.as_f64().unwrap_or(f64::NAN)).collect())
                .unwrap_or_default()
        };
        let vals = floats("values");
        let mut years = floats("years");
        let days: Vec<i64> = match pat.get("days").and_then(Value::as_array) {
            Some(d) if d.len() == vals.len() => d.iter().map(|v| v.as_i64().unwrap_or(-1)).collect(),
            _ => return Err(format!("pattern '{name}' has no dates")),
        };
        if years.len() != vals.len() {
            years = vec![f64::NAN; vals.len()];
        }
        let ok: Vec<usize> = (0..vals.len()).filter(|&i| vals[i].is_finite() && days[i] >= 0).collect();
        if ok.len() < 2 {
            return Err(format!("pattern '{name}' has fewer than 2 valid dates"));
        }
        out.push(Pattern {
            name,
            days: ok.iter().map(|&i| days[i]).collect(),
            values: ok.iter().map(|&i| vals[i]).collect(),
            years: ok.iter().map(|&i| years[i]).collect(),
        });
    }
    Ok(out)
}

/// Dates as zeit.twdtw counts them: days of the year (cycle "year"), else days since 1970.
fn date_numbers(days: &[i64], year_cycle: bool) -> Vec<i32> {
    days.iter()
        .map(|&d| {
            if year_cycle { (d - crate::numeric::year_start(crate::numeric::civil(d).0) + 1) as i32 } else { d as i32 }
        })
        .collect()
}

/// The distance of the series to every pattern (NaN where none), as zeit.twdtw's `distances`.
pub fn distances(p: &Params, values: &[f64], days: &[i64], pats: &[Pattern]) -> Vec<f64> {
    let year_cycle = p.str("cycle") != "none";
    let max_elapsed = p.f64("max_elapsed");
    let params = TwParams {
        alpha: 1.0,
        beta: p.f64("steepness"),
        gamma: p.f64("midpoint"),
        max_time_warp: if max_elapsed > 0.0 { max_elapsed.floor() as i32 } else { i32::MAX },
        subsequence_matching: true,
        cycle_length: if year_cycle { 366.0 } else { 0.0 },
    };
    let dates = date_numbers(days, year_cycle);
    pats.iter()
        .map(|pat| {
            // the pattern sorted by date (stable), as zeit's `_as_pattern`
            let mut order: Vec<usize> = (0..pat.days.len()).collect();
            order.sort_by_key(|&i| pat.days[i]);
            let pv: Vec<f64> = order.iter().map(|&i| pat.values[i]).collect();
            let pd = date_numbers(&order.iter().map(|&i| pat.days[i]).collect::<Vec<_>>(), year_cycle);
            let d = batch(
                values,
                1,
                &dates,
                1,
                Series { values: &pv, dates: &pd, num_bands: 1 },
                &params,
                f64::INFINITY,
                1,
            )[0];
            if d.is_finite() { d } else { f64::NAN }
        })
        .collect()
}

pub fn pixel(p: &Params, input: &PixelInput) -> Result<PixelResult, String> {
    let fail = |why: String| Ok(PixelResult { overlays: vec![], rows: vec![row("TWDTW", why)] });
    let pats = match patterns(p) {
        Ok(v) => v,
        Err(e) => return fail(e),
    };
    if pats.is_empty() {
        return fail("add at least one class pattern (from a pin or the ROI)".into());
    }
    let vals = &input.values;
    let n = vals.iter().filter(|v| v.is_finite()).count();
    let min_valid = p.int("min_valid");
    if (n as i64) < min_valid.max(2) {
        return fail(format!("not enough data ({n} valid, need {min_valid})"));
    }
    let Some(days) = &input.days else {
        return fail("TWDTW needs dates (none were found in the file names or band descriptions)".into());
    };
    let d = distances(p, vals, days, &pats);
    // the class: the first lowest distance; unclassified above the max distance
    let finite = d.iter().any(|x| x.is_finite());
    let best_k = (0..d.len()).fold(0, |b, i| if d[i].is_finite() && (d[b].is_nan() || d[b] > d[i]) { i } else { b });
    let best = if finite { d[best_k] } else { f64::NAN };
    let max_distance = p.f64("max_distance");
    let class = (finite && !(max_distance > 0.0 && best > max_distance)).then_some(best_k);
    let mut sorted: Vec<f64> = d.iter().map(|&x| if x.is_finite() { x } else { f64::INFINITY }).collect();
    sorted.sort_by(f64::total_cmp);
    let second = if pats.len() > 1 { sorted[1] } else { f64::INFINITY };
    let mut rows = vec![];
    if let Some(c) = class {
        rows.push(row("Class", pats[c].name.clone()));
        if second.is_finite() {
            rows.push(row("Margin to 2nd", g(second - best, 4)));
        }
    } else {
        rows.push(row("Class", if finite { "unclassified (above the max distance)" } else { "no match" }));
    }
    let mut order: Vec<usize> = (0..d.len()).collect();
    order.sort_by(|&a, &b| {
        let key = |i: usize| if d[i].is_finite() { d[i] } else { f64::INFINITY };
        key(a).total_cmp(&key(b))
    });
    for i in order {
        rows.push(row(
            format!("  {}", pats[i].name),
            if d[i].is_finite() { g(d[i], 4) } else { "no match (max time apart)".into() },
        ));
    }
    let gaps = vals.len() - n;
    if gaps > 0 {
        rows.push(row("Dates without a value (left out)", gaps.to_string()));
    }
    let mut overlays = vec![];
    if let Some(c) = class {
        let pat = &pats[c];
        let ok: Vec<usize> = (0..pat.years.len()).filter(|&i| pat.years[i].is_finite()).collect();
        if ok.len() >= 2 {
            overlays.push(Overlay::new(
                "line",
                &format!("Pattern: {}", pat.name),
                ok.iter().map(|&i| pat.years[i]).collect(),
                ok.iter().map(|&i| Some(pat.values[i])).collect(),
            ));
        }
    }
    Ok(PixelResult { overlays, rows })
}
