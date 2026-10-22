//! A tool on one pixel's series (the bridge's run_pixel): the series is in memory, the fit is a call. The result
//! is drawn on the chart (overlays) and listed beside it (rows), as the bridge's was.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::manifest::manifest;

/// The parameters of a run by id, every one there (the defaults of those not given).
#[derive(Clone, Debug, Default)]
pub struct Params(pub BTreeMap<String, Value>);

impl Params {
    /// The tool's defaults with `given` over them (unknown ids dropped, as the bridge's `defaults`).
    pub fn with_defaults(tool: &str, given: &BTreeMap<String, Value>) -> Option<Params> {
        let t = manifest().tool(tool)?;
        let mut out: BTreeMap<String, Value> = t.params.iter().map(|p| (p.id.clone(), p.default.clone())).collect();
        for (k, v) in given {
            if let Some(slot) = out.get_mut(k) {
                *slot = v.clone();
            }
        }
        Some(Params(out))
    }

    pub fn f64(&self, id: &str) -> f64 {
        self.0.get(id).and_then(|v| v.as_f64().or_else(|| v.as_bool().map(f64::from))).unwrap_or(f64::NAN)
    }

    /// As Python's int(): a float parameter truncated.
    pub fn int(&self, id: &str) -> i64 {
        self.0.get(id).and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64))).unwrap_or(0)
    }

    pub fn str(&self, id: &str) -> &str {
        self.0.get(id).and_then(Value::as_str).unwrap_or("")
    }

    pub fn bool(&self, id: &str) -> bool {
        self.0.get(id).is_some_and(|v| v.as_bool().unwrap_or_else(|| v.as_f64().is_some_and(|f| f != 0.0)))
    }
}

/// One pixel's series (the bridge's ctx).
#[derive(Clone, Debug, Default)]
pub struct PixelInput {
    /// decimal year of each date
    pub years: Vec<f64>,
    /// days since 1970-01-01 of each date; None when the series has no real dates
    pub days: Option<Vec<i64>>,
    /// NaN = missing
    pub values: Vec<f64>,
}

impl PixelInput {
    /// The calendar year of each date (Zeit's years of the cube's time coordinate).
    pub fn calendar_years(&self) -> Option<Vec<i32>> {
        self.days.as_ref().map(|d| d.iter().map(|&d| civil_year(d)).collect())
    }
}

/// The year of a day counted from 1970-01-01.
pub fn civil_year(days: i64) -> i32 {
    // Howard Hinnant's civil_from_days
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2)) as i32
}

/// A model drawn on the chart: x in decimal years.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Overlay {
    /// "line", "markers" or "vlines"
    #[serde(rename = "type")]
    pub kind: String,
    pub label: String,
    pub x: Vec<f64>,
    /// None: no value there (a gap in the line)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub y: Vec<Option<f64>>,
    /// line width
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<f64>,
    /// opacity (a model's trend drawn lighter under it)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alpha: Option<f64>,
}

impl Overlay {
    pub fn new(kind: &str, label: &str, x: Vec<f64>, y: Vec<Option<f64>>) -> Overlay {
        Overlay { kind: kind.into(), label: label.into(), x, y, ..Default::default() }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct PixelResult {
    pub overlays: Vec<Overlay>,
    /// (label, text) beside the chart
    pub rows: Vec<[String; 2]>,
}

pub(crate) fn row(a: impl Into<String>, b: impl Into<String>) -> [String; 2] {
    [a.into(), b.into()]
}

/// A value as Python's format(v, ".Ng").
pub(crate) fn g(v: f64, n: usize) -> String {
    if v.is_nan() {
        return "nan".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "inf".into() } else { "-inf".into() };
    }
    janus_cube::multidim::format_g(v, n)
}

/// `tool` on one pixel's series.
pub fn run_pixel(tool: &str, p: &Params, input: &PixelInput) -> Result<PixelResult, String> {
    match tool {
        "landtrendr" => crate::landtrendr::pixel(p, input),
        _ => Err(format!("{tool}: not available in this version")),
    }
}
