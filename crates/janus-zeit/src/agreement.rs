//! Agreement of change maps (zeit_bridge/tool_agreement.py): where and when maps of change dates agree
//! (zeit.agreement, zeit/metrics.py, here as it is in Python): each date counts by its calendar year, maps agree
//! when their years are at most `tolerance` apart.

use crate::job::ChunkOut;
use crate::pixel::Params;

/// The maps' values of a band of rows ([map][pixels], NaN no value) -> the outputs.
pub fn chunk(p: &Params, maps: &[Vec<f64>]) -> ChunkOut {
    let tol = p.int("tolerance") as f64;
    let n = maps.first().map_or(0, Vec::len);
    let m = maps.len();
    let names = ["yod", "n_agree", "share", "n_detected", "spread"];
    let mut cols: Vec<Vec<f64>> = vec![Vec::with_capacity(n); names.len()];
    for px in 0..n {
        // a date counts by its year; no event where none
        let y: Vec<f64> =
            maps.iter().map(|a| a[px].floor()).map(|v| if v.is_finite() && v > 0.0 { v } else { f64::NAN }).collect();
        let det: Vec<bool> = y.iter().map(|v| !v.is_nan()).collect();
        // the year most maps agree with; ties: the year more maps give exactly, then the earliest
        let score: Vec<f64> = (0..m)
            .map(|i| {
                if !det[i] {
                    return f64::NEG_INFINITY;
                }
                let support = (0..m).filter(|&o| det[o] && (y[i] - y[o]).abs() <= tol).count() as f64;
                let exact = (0..m).filter(|&o| y[i] == y[o]).count() as f64;
                support * 1e6 + exact * 1e4 - y[i]
            })
            .collect();
        let max = score.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let pick = score.iter().position(|&s| s == max).unwrap_or(0);
        let found = det.iter().any(|&d| d);
        let consensus = if found { if det[pick] { y[pick] } else { 0.0 } } else { f64::NAN };
        let n_agree = (0..m).filter(|&i| det[i] && (y[i] - consensus).abs() <= tol).count() as f64;
        let yod = if consensus.is_nan() { 0.0 } else { (consensus as u16) as f64 };
        let n_detected = det.iter().filter(|&&d| d).count() as f64;
        let (lo, hi) = y
            .iter()
            .filter(|v| !v.is_nan())
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), &v| (a.min(v), b.max(v)));
        let spread = if found { f64::from((hi - lo) as f32) } else { f64::NAN };
        let any_value = maps.iter().any(|a| a[px].is_finite());
        let ok = yod > 0.0;
        let values = [
            if ok { yod } else { f64::NAN },
            if ok { n_agree } else { f64::NAN },
            if ok { 100.0 * n_agree / m as f64 } else { f64::NAN },
            if any_value { n_detected } else { f64::NAN },
            if ok { spread } else { f64::NAN },
        ];
        for (c, v) in cols.iter_mut().zip(values) {
            c.push(v);
        }
    }
    names.iter().map(|n| n.to_string()).zip(cols).collect()
}
