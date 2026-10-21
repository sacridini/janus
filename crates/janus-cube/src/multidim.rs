//! NetCDF, Zarr, HDF and other containers of several variables, each a cube of (time, [level,] y, x)
//! (src/multidim.cpp): the variables a file offers, and where each band of a variable sits on its extra
//! dimensions, with the dates decoded by the CF conventions ("days since 1970-01-01", calendars of climate
//! models). GDAL reads them all: a variable is a subdataset (NETCDF:"f.nc":t2m, ZARR:"f.zarr":/t2m) whose bands are
//! every combination of the extra dimensions; netCDF tags each band with NETCDF_DIM_<dim>=<value> and Zarr with
//! DIM_<dim>_VALUE / DIM_<dim>_UNIT.

use std::collections::HashMap;
use std::ffi::{CStr, CString};

use gdal::{Dataset, Metadata};

use crate::dates::{civil_from_days, days_from_civil, is_leap};
use crate::remote::{dataset_file, gdal_path};

/// One variable (a GDAL subdataset) of a container.
#[derive(Clone, Debug, Default)]
pub struct MdVariable {
    /// the GDAL name that opens it, e.g. NETCDF:"clim.nc":t2m
    pub dataset: String,
    /// e.g. "t2m"
    pub name: String,
    /// e.g. "24 x 50 x 60, 2 m temperature (degC)"
    pub desc: String,
}

/// The bands of a variable on its extra dimensions.
#[derive(Clone, Debug, Default)]
pub struct MdBands {
    /// "" = no time dimension recognised
    pub time_dim: String,
    /// time decoded to dates (CF units)
    pub time_is_date: bool,
    /// per band: Unix seconds if time_is_date, else the coordinate
    pub time: Vec<f64>,
    /// per band: index into `levels`
    pub level: Vec<usize>,
    /// the other dimensions' values, e.g. "level 850" ("" with time alone)
    pub levels: Vec<String>,
    /// CF calendar of the time axis ("" = standard)
    pub calendar: String,
}

#[derive(Clone, Copy, PartialEq)]
enum Calendar {
    Standard,
    NoLeap,
    AllLeap,
    Day360,
}

fn calendar_of(c: &str) -> Calendar {
    match c.trim().to_lowercase().as_str() {
        "noleap" | "365_day" => Calendar::NoLeap,
        "all_leap" | "366_day" => Calendar::AllLeap,
        "360_day" => Calendar::Day360,
        // standard, gregorian, proleptic_gregorian, julian, none, ""
        _ => Calendar::Standard,
    }
}

const CUM: [i64; 13] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334, 365];
const CUM_LEAP: [i64; 13] = [0, 31, 60, 91, 121, 152, 182, 213, 244, 274, 305, 335, 366];

/// Day number of a date in a model calendar (any fixed origin).
fn calendar_days(c: Calendar, y: i32, m: u32, d: u32) -> i64 {
    let (yy, mi, dd) = (i64::from(y), m as usize - 1, i64::from(d));
    match c {
        Calendar::NoLeap => yy * 365 + CUM[mi] + dd - 1,
        Calendar::AllLeap => yy * 366 + CUM_LEAP[mi] + dd - 1,
        Calendar::Day360 => yy * 360 + mi as i64 * 30 + dd - 1,
        Calendar::Standard => days_from_civil(y, m, d),
    }
}

fn calendar_date(c: Calendar, days: i64) -> (i32, u32, u32) {
    if c == Calendar::Standard {
        return civil_from_days(days);
    }
    let len = match c {
        Calendar::NoLeap => 365,
        Calendar::AllLeap => 366,
        _ => 360,
    };
    let y = days.div_euclid(len);
    let r = days - y * len;
    if c == Calendar::Day360 {
        return (y as i32, (r / 30 + 1) as u32, (r % 30 + 1) as u32);
    }
    let cum = if c == Calendar::NoLeap { &CUM } else { &CUM_LEAP };
    let mut m = 1;
    while m < 12 && r >= cum[m] {
        m += 1;
    }
    (y as i32, m as u32, (r - cum[m - 1] + 1) as u32)
}

fn month_length(y: i32, m: u32) -> u32 {
    const LEN: [u32; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if m == 2 && is_leap(y) { 29 } else { LEN[m as usize - 1] }
}

/// A real date for a model calendar's date: the day clamped to the month (30 February -> 28 or 29).
fn unix_of(y: i32, m: u32, d: u32, seconds_of_day: f64) -> f64 {
    let d = d.clamp(1, month_length(y, m));
    days_from_civil(y, m, d) as f64 * 86400.0 + seconds_of_day
}

/// strtod's leading number of `s` and what follows it.
fn leading_number(s: &str) -> Option<(f64, &str)> {
    let b = s.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let digits = i;
    while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
        i += 1;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') && i > digits {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        if j < b.len() && b[j].is_ascii_digit() {
            i = j;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
        }
    }
    if i == digits {
        return None;
    }
    s[..i].parse().ok().map(|v| (v, &s[i..]))
}

/// strtol's leading integer (no fraction).
fn leading_int(s: &str) -> Option<(i64, &str)> {
    let b = s.as_bytes();
    let mut i = usize::from(!b.is_empty() && (b[0] == b'+' || b[0] == b'-'));
    let start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    (i > start).then(|| (s[..i].parse().unwrap_or(0), &s[i..]))
}

/// "1970-01-01", "1900-1-1 00:00:00.0", "2000-01-01T12:00:00Z", "1950-01-01 00:00 -3:00": the date and the
/// seconds of its day (a time zone moves them to UTC).
fn parse_reference(s: &str) -> Option<(i32, u32, u32, f64)> {
    let (y, mut p) = leading_int(s)?;
    let (mut m, mut d) = (1i64, 1i64);
    if let Some(rest) = p.strip_prefix('-') {
        let (v, r) = leading_int(rest).unwrap_or((0, rest));
        m = v;
        p = r;
        if let Some(rest) = p.strip_prefix('-') {
            let (v, r) = leading_int(rest).unwrap_or((0, rest));
            d = v;
            p = r;
        }
    }
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let mut sod = 0.0;
    p = p.trim_start_matches([' ', 'T', 't']);
    if p.starts_with(|c: char| c.is_ascii_digit())
        && let Some((h, r)) = leading_number(p)
    {
        p = r;
        let (mut mi, mut sec) = (0.0, 0.0);
        if let Some(rest) = p.strip_prefix(':') {
            let (v, r) = leading_number(rest).unwrap_or((0.0, rest));
            mi = v;
            p = r;
            if let Some(rest) = p.strip_prefix(':') {
                let (v, r) = leading_number(rest).unwrap_or((0.0, rest));
                sec = v;
                p = r;
            }
        }
        sod = h * 3600.0 + mi * 60.0 + sec;
        p = p.trim_start_matches(' ');
        // a time zone: the reference is local time
        if let Some(sign) = p.chars().next().filter(|c| *c == '+' || *c == '-') {
            let sign = if sign == '-' { -1.0 } else { 1.0 };
            if let Some((th, r)) = leading_number(&p[1..]) {
                let tm = r.strip_prefix(':').and_then(leading_number).map_or(0.0, |v| v.0);
                if th.abs() <= 14.0 {
                    sod -= sign * (th.abs() * 3600.0 + tm * 60.0);
                }
            }
        }
    }
    Some((y as i32, m as u32, d as u32, sod))
}

/// A CF time value -> Unix seconds. units: "<unit> since <date>[ <time>]"; calendar: standard / gregorian /
/// proleptic_gregorian (also "" and julian, as Gregorian), noleap / 365_day, all_leap / 366_day, 360_day. Dates of
/// the model calendars become the nearest real date. "months since" / "years since" with whole values count
/// calendar months and years (as in monthly and yearly products). None if `units` is not CF time.
pub fn cf_time_to_unix(value: f64, units: &str, calendar: &str) -> Option<f64> {
    if !value.is_finite() {
        return None;
    }
    let since = units.to_lowercase().find(" since ")?;
    let unit = units[..since].trim().to_lowercase();
    let (y, m, d, sod) = parse_reference(units[since + 7..].trim())?;
    let cal = calendar_of(calendar);
    let months = matches!(unit.as_str(), "month" | "months" | "mon");
    let years = matches!(unit.as_str(), "year" | "years" | "yr" | "yrs");
    if (months || years) && (value - value.round()).abs() < 1e-6 {
        // whole months / years: calendar steps
        let total = i64::from(y) * 12 + i64::from(m) - 1 + (value.round() as i64) * if years { 12 } else { 1 };
        let y2 = total.div_euclid(12);
        let m2 = (total - y2 * 12) as u32 + 1;
        return Some(unix_of(y2 as i32, m2, d, sod));
    }
    let step = match unit.as_str() {
        "s" | "sec" | "secs" | "second" | "seconds" => 1.0,
        "ms" | "msec" | "msecs" | "millisecond" | "milliseconds" => 0.001,
        "min" | "mins" | "minute" | "minutes" => 60.0,
        "h" | "hr" | "hrs" | "hour" | "hours" => 3600.0,
        "d" | "day" | "days" => 86400.0,
        "week" | "weeks" => 7.0 * 86400.0,
        // udunits' month and year
        _ if months => 365.242198781 / 12.0 * 86400.0,
        _ if years => 365.242198781 * 86400.0,
        _ => return None,
    };
    let total = calendar_days(cal, y, m, d) as f64 * 86400.0 + sod + value * step;
    let days = (total / 86400.0).floor() as i64;
    let rest = total - days as f64 * 86400.0;
    if cal == Calendar::Standard {
        return Some(days as f64 * 86400.0 + rest);
    }
    let (y2, m2, d2) = calendar_date(cal, days);
    Some(unix_of(y2, m2, d2, rest))
}

/// The container a variable comes from, as GDAL opens it: the file of NETCDF:"f.nc":t2m; ZARR:"store" for a Zarr
/// array (a remote store needs the prefix).
pub fn container_of(dataset: &str) -> String {
    if let Some(rest) = dataset.strip_prefix("ZARR:\"")
        && let Some(end) = rest.find('"')
    {
        return dataset[..6 + end + 1].to_string();
    }
    dataset_file(dataset).to_string()
}

/// A subdataset name with a variable after the file?
fn names_variable(path: &str) -> bool {
    let file = dataset_file(path);
    if file == path {
        return false;
    }
    let colon = path.find(':').unwrap_or(0);
    path[colon + 2..].find('"').is_some_and(|e| colon + 2 + e + 1 < path.len())
}

/// A multidimensional view of a container, through GDAL's C API (the gdal crate does not open arrays by their
/// full name): the attributes and units of its arrays.
struct Multidim(gdal_sys::GDALDatasetH);

impl Multidim {
    fn open(container: &str) -> Option<Multidim> {
        let c = CString::new(container).ok()?;
        // gdal.h's GDAL_OF_MULTIDIM_RASTER | GDAL_OF_READONLY (macros, not in the bindings)
        let flags = 0x10;
        // SAFETY: a C string and null option lists; the handle is closed in Drop.
        let h =
            unsafe { gdal_sys::GDALOpenEx(c.as_ptr(), flags, std::ptr::null(), std::ptr::null(), std::ptr::null()) };
        (!h.is_null()).then_some(Multidim(h))
    }

    /// An array's attribute as text: the array by its full name ("/t2m") first, then within the root.
    fn attribute(&self, array: &str, attr: &str) -> Option<String> {
        let a = CString::new(attr).ok()?;
        self.with_array(array, |arr| {
            // SAFETY: handles from GDAL, released below; the string belongs to the attribute.
            unsafe {
                let at = gdal_sys::GDALMDArrayGetAttribute(arr, a.as_ptr());
                if at.is_null() {
                    return None;
                }
                let s = gdal_sys::GDALAttributeReadAsString(at);
                let out = (!s.is_null()).then(|| CStr::from_ptr(s).to_string_lossy().into_owned());
                gdal_sys::GDALAttributeRelease(at);
                out
            }
        })
        .flatten()
    }

    fn unit(&self, array: &str) -> Option<String> {
        self.with_array(array, |arr| {
            // SAFETY: the unit string belongs to the array.
            unsafe {
                let s = gdal_sys::GDALMDArrayGetUnit(arr);
                (!s.is_null()).then(|| CStr::from_ptr(s).to_string_lossy().into_owned())
            }
        })
        .flatten()
    }

    fn with_array<R>(&self, array: &str, f: impl FnOnce(gdal_sys::GDALMDArrayH) -> R) -> Option<R> {
        let full = CString::new(format!("/{}", array.trim_start_matches('/'))).ok()?;
        let plain = CString::new(array).ok()?;
        // SAFETY: handles from GDAL, released before returning.
        unsafe {
            let root = gdal_sys::GDALDatasetGetRootGroup(self.0);
            if root.is_null() {
                return None;
            }
            let mut arr = gdal_sys::GDALGroupOpenMDArrayFromFullname(root, full.as_ptr(), std::ptr::null_mut());
            if arr.is_null() {
                arr = gdal_sys::GDALGroupOpenMDArray(root, plain.as_ptr(), std::ptr::null_mut());
            }
            gdal_sys::GDALGroupRelease(root);
            if arr.is_null() {
                return None;
            }
            let r = f(arr);
            gdal_sys::GDALMDArrayRelease(arr);
            Some(r)
        }
    }
}

impl Drop for Multidim {
    fn drop(&mut self) {
        // SAFETY: opened by GDALOpenEx.
        unsafe { gdal_sys::GDALClose(self.0) };
    }
}

/// The variables of a container with several of them (a single-variable NetCDF opens directly and lists
/// nothing); empty if `input` is not such a file.
pub fn list_variables(input: &str) -> Vec<MdVariable> {
    let path = gdal_path(input);
    if names_variable(&path) {
        return Vec::new();
    }
    let Ok(ds) = crate::open_raster(&path) else { return Vec::new() };
    if ds.raster_count() > 0 {
        return Vec::new(); // a single variable (or an ordinary raster): opens directly
    }
    let sds: HashMap<String, String> = ds
        .metadata_domain("SUBDATASETS")
        .unwrap_or_default()
        .into_iter()
        .filter_map(|kv| kv.split_once('=').map(|(k, v)| (k.to_uppercase(), v.to_string())))
        .collect();
    let mut out = Vec::new();
    for i in 1.. {
        let Some(name) = sds.get(&format!("SUBDATASET_{i}_NAME")) else { break };
        let desc = sds.get(&format!("SUBDATASET_{i}_DESC")).cloned().unwrap_or_default();
        let mut v = MdVariable { dataset: name.clone(), ..Default::default() };
        v.name = name.rsplit(':').next().unwrap_or(name).trim_start_matches('/').to_string();
        // shape: "[24x50x60] t2m (32-bit floating-point)" -> 24 x 50 x 60
        let mut shape = Vec::new();
        if let Some(rest) = desc.strip_prefix('[') {
            let mut p = rest;
            while let Some((n, r)) = leading_int(p) {
                shape.push(n);
                match r.strip_prefix('x') {
                    Some(r) => p = r,
                    None => break,
                }
            }
        }
        // not a raster: bounds of cells (n x 2), coordinate reference variables
        let l = v.name.to_lowercase();
        if (shape.len() >= 2 && *shape.last().unwrap() <= 2)
            || l.ends_with("_bnds")
            || l.ends_with("_bounds")
            || l == "crs"
            || l == "spatial_ref"
        {
            continue;
        }
        v.desc = shape.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(" x ");
        if v.desc.is_empty() {
            v.desc = desc;
        }
        out.push(v);
    }
    // long names and units, from the multidimensional view of the container
    if let Some(md) = out.first().and_then(|v| Multidim::open(&container_of(&v.dataset))) {
        for v in &mut out {
            let mut long = md.attribute(&v.name, "long_name").unwrap_or_default();
            if long.is_empty() {
                long = md.attribute(&v.name, "standard_name").unwrap_or_default();
            }
            let mut units = md.attribute(&v.name, "units").unwrap_or_default();
            if units.is_empty() {
                units = md.unit(&v.name).unwrap_or_default(); // Zarr: the array's unit
            }
            if !long.is_empty() {
                v.desc += &format!(", {long}");
            }
            if !units.is_empty() {
                v.desc += &format!(" ({units})");
            }
        }
    }
    out
}

/// A number as C's %.<precision>g.
pub fn format_g(v: f64, precision: usize) -> String {
    if v == 0.0 || !v.is_finite() {
        return format!("{v}");
    }
    let p = precision.max(1);
    // %g picks %e when the exponent of the value rounded to p digits is below -4 or at least p
    let rounded = format!("{v:.*e}", p - 1);
    let (mantissa, exp) = rounded.split_once('e').unwrap_or((&rounded, "0"));
    let e: i32 = exp.parse().unwrap_or(0);
    if e >= -4 && e < p as i32 {
        let decimals = (p as i32 - 1 - e).max(0) as usize;
        let f = format!("{v:.decimals$}");
        if f.contains('.') { f.trim_end_matches('0').trim_end_matches('.').to_string() } else { f }
    } else {
        let m = if mantissa.contains('.') { mantissa.trim_end_matches('0').trim_end_matches('.') } else { mantissa };
        format!("{m}e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs())
    }
}

/// A number as C's %.6g.
pub fn format_g6(v: f64) -> String {
    format_g(v, 6)
}

/// A coordinate as %.6g (text that is not a number, as it is).
fn format_coordinate(raw: &str) -> String {
    leading_number(raw).map_or_else(|| raw.to_string(), |(v, _)| format_g6(v))
}

/// Where each band of a variable sits on its extra dimensions; None if the bands carry no dimension tags (an
/// ordinary multiband file).
pub fn read_md_bands(ds: &Dataset, path: &str) -> Option<MdBands> {
    let n = ds.raster_count();
    if n < 1 {
        return None;
    }
    // per band: (dim, value)
    let mut coords: Vec<Vec<(String, String)>> = vec![Vec::new(); n];
    let mut units: HashMap<String, String> = HashMap::new();
    for (b, c) in coords.iter_mut().enumerate() {
        let rb = ds.rasterband(b + 1).ok()?;
        for kv in rb.metadata_domain("").unwrap_or_default() {
            let Some((k, v)) = kv.split_once('=') else { continue };
            if let Some(dim) = k.strip_prefix("NETCDF_DIM_") {
                c.push((dim.to_string(), v.to_string()));
            } else if let Some(dim) = k.strip_prefix("DIM_").and_then(|r| r.strip_suffix("_VALUE")) {
                c.push((dim.to_string(), v.to_string()));
            } else if let Some(dim) = k.strip_prefix("DIM_").and_then(|r| r.strip_suffix("_UNIT")) {
                units.insert(dim.to_string(), v.to_string());
            }
        }
    }
    if coords[0].is_empty() {
        return None;
    }
    let dims: Vec<String> = coords[0].iter().map(|c| c.0.clone()).collect();
    if coords.iter().any(|c| c.len() != dims.len()) {
        return None; // not one value per dimension on every band
    }
    let unit_of = |dim: &str| {
        ds.metadata_item(&format!("{dim}#units"), "").or_else(|| units.get(dim).cloned()).unwrap_or_default()
    };
    // the time axis: CF units ("... since ..."), else a usual name, else the first dimension
    let ti = dims
        .iter()
        .position(|d| unit_of(d).to_lowercase().contains(" since "))
        .or_else(|| {
            dims.iter()
                .position(|d| matches!(d.to_lowercase().as_str(), "time" | "t" | "date" | "valid_time" | "times"))
        })
        .unwrap_or(0);
    let mut out = MdBands { time_dim: dims[ti].clone(), ..Default::default() };
    let tunits = unit_of(&out.time_dim);
    let is_cf = tunits.to_lowercase().contains(" since ");
    out.calendar = match ds.metadata_item(&format!("{}#calendar", out.time_dim), "") {
        Some(c) => c,
        None if is_cf => {
            Multidim::open(&container_of(path)).and_then(|m| m.attribute(&out.time_dim, "calendar")).unwrap_or_default()
        }
        None => String::new(),
    };
    out.time_is_date = is_cf;
    let mut level_index: HashMap<String, usize> = HashMap::new();
    for c in &coords {
        let v = leading_number(&c[ti].1).map_or(0.0, |x| x.0);
        let mut t = v;
        if out.time_is_date {
            match cf_time_to_unix(v, &tunits, &out.calendar) {
                Some(u) => t = u,
                None => out.time_is_date = false,
            }
        }
        out.time.push(t);
        let lv = c
            .iter()
            .enumerate()
            .filter(|(k, _)| *k != ti)
            .map(|(_, (d, v))| format!("{d} {}", format_coordinate(v)))
            .collect::<Vec<_>>()
            .join(", ");
        let next = out.levels.len();
        let idx = *level_index.entry(lv.clone()).or_insert_with(|| {
            out.levels.push(lv);
            next
        });
        out.level.push(idx);
    }
    if !out.time_is_date {
        // undecodable: the raw coordinate
        out.time = coords.iter().map(|c| leading_number(&c[ti].1).map_or(0.0, |x| x.0)).collect();
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(y: i32, m: u32, d: u32) -> f64 {
        days_from_civil(y, m, d) as f64 * 86400.0
    }

    #[test]
    fn cf_times() {
        assert_eq!(cf_time_to_unix(1.0, "days since 1970-01-01", ""), Some(86400.0));
        assert_eq!(
            cf_time_to_unix(12.0, "hours since 2000-01-01 00:00:00", "standard"),
            Some(day(2000, 1, 1) + 43200.0)
        );
        // monthly products: calendar months
        assert_eq!(cf_time_to_unix(13.0, "months since 2000-01-15", ""), Some(day(2001, 2, 15)));
        // a 360-day calendar's 30 February is the last day of February
        assert_eq!(cf_time_to_unix(59.0, "days since 2001-01-01", "360_day"), Some(day(2001, 2, 28)));
        // noleap: day 59 of 2000 is 1 March
        assert_eq!(cf_time_to_unix(59.0, "days since 2000-01-01", "noleap"), Some(day(2000, 3, 1)));
        // a time zone in the reference
        assert_eq!(
            cf_time_to_unix(0.0, "hours since 1950-01-01 00:00 -3:00", ""),
            Some(day(1950, 1, 1) + 3.0 * 3600.0)
        );
        assert_eq!(cf_time_to_unix(1.0, "meters", ""), None);
    }

    #[test]
    fn names_and_coordinates() {
        assert_eq!(container_of(r#"ZARR:"/vsis3/b/x.zarr":/t2m"#), r#"ZARR:"/vsis3/b/x.zarr""#);
        assert_eq!(container_of(r#"NETCDF:"clim.nc":t2m"#), "clim.nc");
        assert!(names_variable(r#"NETCDF:"clim.nc":t2m"#));
        assert!(!names_variable(r"C:\x\clim.nc"));
        assert_eq!(format_coordinate("850"), "850");
        assert_eq!(format_coordinate("0.123456789"), "0.123457");
        assert_eq!(format_coordinate("1500000"), "1.5e+06");
        assert_eq!(format_g6(0.0001), "0.0001");
        assert_eq!(format_g6(0.00001), "1e-05");
        assert_eq!(format_g6(-273.15), "-273.15");
    }
}
