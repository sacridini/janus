//! How a raster's values become classes, the names of intervals, a raster's grid and the area of its pixels
//! (src/validation.cpp: MapRule, intervalName, parseBins, Grid, rowAreas).

use std::collections::{BTreeMap, BTreeSet};

use gdal::Dataset;
use gdal::spatial_ref::SpatialRef;
use serde_json::{Value, json};

use crate::NONE;

/// How a raster's values become classes.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MapKind {
    /// its integer values (a land-cover map, a class map of Zeit)
    #[default]
    Classes,
    /// intervals of its values (bins: edges, [a, b)), e.g. a magnitude
    Intervals,
    /// a date of change (year of detection): > 0 change, else no change; bins: a class per period
    Events,
}

impl MapKind {
    pub fn name(self) -> &'static str {
        match self {
            MapKind::Classes => "classes",
            MapKind::Intervals => "intervals",
            MapKind::Events => "events",
        }
    }
}

/// A raster and the rule that turns its values into classes.
#[derive(Clone, Debug, PartialEq)]
pub struct MapRule {
    /// GDAL path
    pub path: String,
    pub band: i32,
    /// as shown
    pub name: String,
    pub kind: MapKind,
    /// Intervals: edges; Events: year edges (empty: change / no change)
    pub bins: Vec<f64>,
    /// Classes: value -> name (else the value)
    pub names: BTreeMap<i32, String>,
    /// Classes: values that are change; Intervals: intervals (1-based)
    pub change: BTreeSet<i32>,
}

impl Default for MapRule {
    fn default() -> Self {
        MapRule {
            path: String::new(),
            band: 1,
            name: String::new(),
            kind: MapKind::Classes,
            bins: Vec::new(),
            names: BTreeMap::new(),
            change: BTreeSet::new(),
        }
    }
}

/// The index of the first edge above v (C++'s upper_bound).
fn upper_bound(bins: &[f64], v: f64) -> usize {
    bins.partition_point(|&b| b <= v)
}

impl MapRule {
    /// Classes: the value; Intervals: 1.. (the interval); Events: 0 no change, 1.. change (its period; 1 without
    /// bins). NONE: none (no data, outside the intervals).
    pub fn class_of(&self, v: f64) -> i32 {
        match self.kind {
            MapKind::Classes => {
                if !v.is_finite() || v != v.floor() || v.abs() > 2e9 {
                    return NONE;
                }
                v as i32
            }
            MapKind::Intervals => {
                if !v.is_finite() {
                    return NONE;
                }
                let i = upper_bound(&self.bins, v);
                if i >= 1 && i < self.bins.len() { i as i32 } else { NONE }
            }
            MapKind::Events => {
                if !v.is_finite() || v <= 0.0 {
                    return 0; // no data is no change, as zeit's maps of events
                }
                if self.bins.is_empty() {
                    return 1;
                }
                let i = upper_bound(&self.bins, v);
                if i >= 1 && i < self.bins.len() { i as i32 } else { NONE }
            }
        }
    }

    pub fn is_change(&self, cls: i32) -> bool {
        if cls == NONE {
            return false;
        }
        if self.kind == MapKind::Events {
            return cls > 0;
        }
        self.change.contains(&cls)
    }

    pub fn class_name(&self, cls: i32) -> String {
        if cls == NONE {
            return String::new();
        }
        let n = self.bins.len() as i32;
        match self.kind {
            MapKind::Classes => match self.names.get(&cls) {
                Some(s) if !s.is_empty() => s.clone(),
                _ => cls.to_string(),
            },
            MapKind::Intervals => {
                if cls >= 1 && cls < n {
                    interval_name(self.bins[cls as usize - 1], self.bins[cls as usize], false)
                } else {
                    String::new()
                }
            }
            MapKind::Events => {
                if cls == 0 {
                    "no change".into()
                } else if self.bins.is_empty() {
                    "change".into()
                } else if cls >= 1 && cls < n {
                    interval_name(self.bins[cls as usize - 1], self.bins[cls as usize], true)
                } else {
                    String::new()
                }
            }
        }
    }

    pub fn to_json(&self) -> Value {
        let names: serde_json::Map<String, Value> =
            self.names.iter().map(|(v, s)| (v.to_string(), Value::String(s.clone()))).collect();
        json!({
            "path": self.path,
            "band": self.band,
            "name": self.name,
            "kind": self.kind.name(),
            "bins": self.bins,
            "names": names,
            "change": self.change.iter().collect::<Vec<_>>(),
        })
    }

    pub fn from_json(j: &Value) -> MapRule {
        let mut r = MapRule {
            path: str_of(j, "path"),
            band: j.get("band").and_then(Value::as_i64).unwrap_or(1) as i32,
            name: str_of(j, "name"),
            ..Default::default()
        };
        r.kind = match j.get("kind").and_then(Value::as_str).unwrap_or("classes") {
            "intervals" => MapKind::Intervals,
            "events" => MapKind::Events,
            _ => MapKind::Classes,
        };
        for b in j.get("bins").and_then(Value::as_array).into_iter().flatten() {
            if let Some(v) = b.as_f64() {
                r.bins.push(v);
            }
        }
        for (v, s) in j.get("names").and_then(Value::as_object).into_iter().flatten() {
            if let Some(s) = s.as_str() {
                r.names.insert(atoi(v), s.to_string());
            }
        }
        for c in j.get("change").and_then(Value::as_array).into_iter().flatten() {
            if let Some(c) = c.as_i64() {
                r.change.insert(c as i32);
            }
        }
        r
    }
}

/// A string member of a JSON object ("" when absent).
pub(crate) fn str_of(j: &Value, key: &str) -> String {
    j.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

/// C's atoi: an optional sign and the digits that follow, 0 when none.
pub(crate) fn atoi(s: &str) -> i32 {
    let s = s.trim_start();
    let (neg, rest) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    let v = digits.parse::<i64>().unwrap_or(0);
    (if neg { -v } else { v }) as i32
}

/// printf's %.{prec}g.
pub fn fmt_g(v: f64, prec: usize) -> String {
    if v.is_nan() {
        return "nan".into();
    }
    if v.is_infinite() {
        return if v < 0.0 { "-inf".into() } else { "inf".into() };
    }
    if v == 0.0 {
        return if v.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    let p = prec.max(1);
    // the exponent after rounding to p significant digits
    let sci = format!("{:.*e}", p - 1, v);
    let (mant, exp) = sci.split_once('e').unwrap();
    let x: i32 = exp.parse().unwrap();
    let trim = |s: &str| -> String {
        if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s.to_string() }
    };
    if x < -4 || x >= p as i32 {
        format!("{}e{}{:02}", trim(mant), if x < 0 { '-' } else { '+' }, x.abs())
    } else {
        trim(&format!("{:.*}", (p as i32 - 1 - x).max(0) as usize, v))
    }
}

/// As zeit's names of intervals: "0.1-0.5"; years "2000-2009", or "2005" for one year.
pub fn interval_name(lo: f64, hi: f64, years: bool) -> String {
    if years {
        return if hi - lo > 1.0 { format!("{}-{}", lo as i32, hi as i32 - 1) } else { format!("{}", lo as i32) };
    }
    let f = |v: f64| {
        if v.is_infinite() {
            return if v < 0.0 { "-inf".to_string() } else { "inf".to_string() };
        }
        if v == v.floor() && v.abs() < 1e15 { format!("{}", v as i64) } else { fmt_g(v, 6) }
    };
    format!("{}-{}", f(lo), f(hi))
}

/// "0, 0.5, 1" -> edges (increasing; "inf" and "-inf" allowed).
pub fn parse_bins(text: &str) -> Result<Vec<f64>, String> {
    let t = text.replace([';', '\t'], ",");
    let mut out = Vec::new();
    for tok in t.split(',') {
        let tok = tok.trim_matches(' ');
        if tok.is_empty() {
            continue;
        }
        match tok.parse::<f64>() {
            Ok(v) => out.push(v),
            Err(_) => return Err(format!("not a number: {tok}")),
        }
    }
    if out.windows(2).any(|w| !(w[1] > w[0])) {
        return Err("the edges must increase".into());
    }
    if out.len() == 1 {
        return Err("give at least two edges".into());
    }
    Ok(out)
}

pub fn bins_text(bins: &[f64]) -> String {
    bins.iter().map(|&b| fmt_g(b, 6)).collect::<Vec<_>>().join(", ")
}

/// A raster's grid: size, transform and CRS.
#[derive(Clone, Debug, PartialEq)]
pub struct Grid {
    pub w: usize,
    pub h: usize,
    pub gt: [f64; 6],
    pub has_gt: bool,
    /// "" = none
    pub wkt: String,
}

impl Default for Grid {
    fn default() -> Self {
        Grid { w: 0, h: 0, gt: [0.0, 1.0, 0.0, 0.0, 0.0, 1.0], has_gt: false, wkt: String::new() }
    }
}

impl Grid {
    /// The centre of pixel (col, row) in the grid's CRS.
    pub fn centre(&self, col: i64, row: i64) -> (f64, f64) {
        let g = &self.gt;
        let (c, r) = (col as f64 + 0.5, row as f64 + 0.5);
        (g[0] + c * g[1] + r * g[2], g[3] + c * g[4] + r * g[5])
    }

    pub fn to_json(&self) -> Value {
        json!({"w": self.w, "h": self.h, "gt": self.gt, "has_gt": self.has_gt, "wkt": self.wkt})
    }

    pub fn from_json(j: &Value) -> Grid {
        let mut g = Grid {
            w: j.get("w").and_then(Value::as_u64).unwrap_or(0) as usize,
            h: j.get("h").and_then(Value::as_u64).unwrap_or(0) as usize,
            has_gt: j.get("has_gt").and_then(Value::as_bool).unwrap_or(false),
            wkt: str_of(j, "wkt"),
            ..Default::default()
        };
        if let Some(t) = j.get("gt").and_then(Value::as_array) {
            for (i, v) in t.iter().take(6).enumerate() {
                g.gt[i] = v.as_f64().unwrap_or(0.0);
            }
        }
        g
    }
}

/// A CRS as WKT2 (2018), as the C++ writes it in projects ("" if none).
pub(crate) fn wkt_of(srs: &SpatialRef) -> String {
    use std::ffi::{CStr, CString};
    let opt = CString::new("FORMAT=WKT2_2018").unwrap();
    let opts = [opt.as_ptr(), std::ptr::null()];
    let mut out: *mut std::ffi::c_char = std::ptr::null_mut();
    // SAFETY: the handle belongs to `srs`; GDAL allocates the string, freed with VSIFree.
    unsafe {
        if gdal_sys::OSRExportToWktEx(srs.to_c_hsrs(), &mut out, opts.as_ptr()) != 0 || out.is_null() {
            if !out.is_null() {
                gdal_sys::VSIFree(out as *mut std::ffi::c_void);
            }
            return srs.to_wkt().unwrap_or_default();
        }
        let s = CStr::from_ptr(out).to_string_lossy().into_owned();
        gdal_sys::VSIFree(out as *mut std::ffi::c_void);
        s
    }
}

pub(crate) fn grid_of(ds: &Dataset) -> Grid {
    let (w, h) = ds.raster_size();
    let mut g = Grid { w, h, ..Default::default() };
    if let Ok(t) = ds.geo_transform() {
        g.gt = t;
        g.has_gt = true;
    }
    g.wkt = ds.spatial_ref().map(|s| wkt_of(&s)).unwrap_or_default();
    g
}

/// Two CRSs are the same (one without a CRS is taken as the other's).
pub(crate) fn same_crs(a: &str, b: &str) -> bool {
    if a.is_empty() || b.is_empty() || a == b {
        return true;
    }
    match (SpatialRef::from_wkt(a), SpatialRef::from_wkt(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

pub(crate) fn same_grid(a: &Grid, b: &Grid) -> bool {
    if a.w != b.w || a.h != b.h || a.has_gt != b.has_gt {
        return false;
    }
    let tol = 1e-6 * a.gt[1].abs().max(a.gt[5].abs());
    if (0..6).any(|i| (a.gt[i] - b.gt[i]).abs() > tol) {
        return false;
    }
    same_crs(&a.wkt, &b.wkt)
}

pub fn read_grid(path: &str) -> Result<Grid, String> {
    let ds = crate::reader::open_raster(path, 1)?;
    Ok(grid_of(&ds))
}

/// The area of a pixel on each row: hectares (the sphere's, with the authalic radius, in a geographic CRS), or
/// 1 per pixel without a CRS (unit "pixels").
pub fn row_areas(g: &Grid) -> (Vec<f64>, String) {
    let srs = if g.wkt.is_empty() || !g.has_gt { None } else { SpatialRef::from_wkt(&g.wkt).ok() };
    let Some(s) = srs else { return (vec![1.0; g.h], "pixels".into()) };
    let a = if s.is_geographic() {
        let (r, rad) = (6371007.181, std::f64::consts::PI / 180.0);
        (0..g.h)
            .map(|y| {
                let top = (g.gt[3] + g.gt[5] * y as f64) * rad;
                let bottom = (g.gt[3] + g.gt[5] * (y + 1) as f64) * rad;
                r * r * g.gt[1].abs() * rad * (top.sin() - bottom.sin()).abs() / 1e4
            })
            .collect()
    } else {
        let f = s.linear_units();
        vec![(g.gt[1] * g.gt[5] - g.gt[2] * g.gt[4]).abs() * f * f / 1e4; g.h]
    };
    (a, "ha".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printf_g() {
        assert_eq!(fmt_g(0.1, 6), "0.1");
        assert_eq!(fmt_g(1234567.0, 6), "1.23457e+06");
        assert_eq!(fmt_g(0.0001, 6), "0.0001");
        assert_eq!(fmt_g(0.00001, 6), "1e-05");
        assert_eq!(fmt_g(100000.0, 6), "100000");
        assert_eq!(fmt_g(-2.5, 6), "-2.5");
        assert_eq!(fmt_g(0.95, 6), "0.95");
        assert_eq!(fmt_g(0.1, 17), "0.10000000000000001");
        assert_eq!(fmt_g(2010.0, 17), "2010");
    }

    #[test]
    fn names_and_bins() {
        assert_eq!(interval_name(0.1, 0.5, false), "0.1-0.5");
        assert_eq!(interval_name(f64::NEG_INFINITY, 0.0, false), "-inf-0");
        assert_eq!(interval_name(2000.0, 2010.0, true), "2000-2009");
        assert_eq!(interval_name(2005.0, 2006.0, true), "2005");
        assert_eq!(parse_bins("0; 0.5,1").unwrap(), vec![0.0, 0.5, 1.0]);
        assert_eq!(parse_bins("-inf, 0, inf").unwrap(), vec![f64::NEG_INFINITY, 0.0, f64::INFINITY]);
        assert!(parse_bins("1, 0").is_err() && parse_bins("1").is_err() && parse_bins("a, 1").is_err());
        assert_eq!(bins_text(&[0.0, 0.5, f64::INFINITY]), "0, 0.5, inf");
        assert_eq!(atoi("-12x"), -12);
        assert_eq!(atoi("x"), 0);
    }

    #[test]
    fn classes() {
        let r = MapRule { kind: MapKind::Events, bins: vec![2000.0, 2010.0, 2021.0], ..Default::default() };
        assert_eq!(r.class_of(f64::NAN), 0);
        assert_eq!(r.class_of(2004.0), 1);
        assert_eq!(r.class_of(2020.5), 2);
        assert_eq!(r.class_of(2021.0), NONE);
        assert_eq!(r.class_name(2), "2010-2020");
        let c = MapRule { change: [2].into(), ..Default::default() };
        assert_eq!(c.class_of(2.0), 2);
        assert_eq!(c.class_of(2.5), NONE);
        assert!(c.is_change(2) && !c.is_change(1));
    }
}
