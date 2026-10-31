//! One file of several bands a date, date after date, or of one band a date whose dates are not in its bands'
//! names: the dates from the bands' names (zeit.save_raster's time x band stack, "2020-01-15_red"), from Zeit's
//! ZEIT_TIME tag, or from a `<name>_dates.csv` next to it (a date a row, as Zeit and cdts write it: a CCDC stack of
//! 125 dates x 7 bands in 875 bands).

use std::path::{Path, PathBuf};

use gdal::{Dataset, Metadata};

use crate::dates::{parse_date, split_date};

pub(crate) struct DateGroups {
    /// each date's text, in the file's order
    pub dates: Vec<String>,
    /// the bands of a date
    pub bands: usize,
    /// their names ("Band k" when the file has none)
    pub names: Vec<String>,
    /// where the dates were found ("" the bands' names)
    pub source: String,
}

/// The dates of a single file's `n` bands, in groups of the same number of bands, when they are not one a band in
/// its bands' names (None: as before, a band a date by their names, or no dates).
pub(crate) fn date_groups(ds: &Dataset, path: &str, n: usize) -> Option<DateGroups> {
    // a container's variable has its own coordinates
    if matches!(ds.driver().short_name().as_str(), "netCDF" | "Zarr" | "HDF5" | "HDF4" | "HDF5Image" | "HDF4Image") {
        return None;
    }
    let descs: Vec<String> = (1..=n)
        .map(|b| ds.rasterband(b).ok().and_then(|rb| rb.description().ok()).unwrap_or_default().trim().to_string())
        .collect();
    let split: Vec<Option<((f64, _), String)>> = descs.iter().map(|d| split_date(d)).collect();
    if let Some(g) = by_names(&split) {
        return Some(g);
    }
    if split.iter().all(Option::is_some) {
        // a date a band, named so
        return None;
    }
    let names = |d: usize| -> Vec<String> {
        if descs[..d].iter().all(|s| !s.is_empty()) {
            descs[..d].to_vec()
        } else {
            (1..=d).map(|k| format!("Band {k}")).collect()
        }
    };
    let groups = |dates: Vec<String>, source: String| {
        let t = dates.len();
        (t >= 2 && n.is_multiple_of(t)).then(|| DateGroups { bands: n / t, names: names(n / t), dates, source })
    };
    // Zeit's tag: a JSON list of ISO dates
    if let Some(tag) = ds.metadata_item("ZEIT_TIME", "")
        && let Ok(serde_json::Value::Array(list)) = serde_json::from_str(&tag)
    {
        let dates: Option<Vec<String>> =
            list.iter().map(|v| v.as_str().filter(|s| parse_date(s).is_some()).map(str::to_string)).collect();
        if let Some(g) = dates.and_then(|d| groups(d, "the file's ZEIT_TIME".into())) {
            return Some(g);
        }
    }
    let (file, dates) = sidecar(path)?;
    groups(dates, file.file_name()?.to_string_lossy().into_owned())
}

/// Names "<date>_<band>" date after date, the same bands each date in the same order.
fn by_names(split: &[Option<((f64, crate::TimeGranularity), String)>]) -> Option<DateGroups> {
    let parts: Vec<&((f64, _), String)> = split.iter().map(|s| s.as_ref()).collect::<Option<_>>()?;
    let n = parts.len();
    let d = parts.iter().take_while(|p| p.0.0 == parts[0].0.0).count();
    // two dates at least, each of the bands of different names
    if d < 2 || d == n || !n.is_multiple_of(d) || parts.iter().any(|p| p.1.is_empty()) {
        return None;
    }
    let mut names: Vec<&String> = parts[..d].iter().map(|p| &p.1).collect();
    names.sort();
    names.dedup();
    if names.len() != d {
        return None;
    }
    let same = (0..n).all(|b| parts[b].0.0 == parts[b / d * d].0.0 && parts[b].1 == parts[b % d].1);
    let mut times: Vec<f64> = (0..n / d).map(|g| parts[g * d].0.0).collect();
    times.sort_by(f64::total_cmp);
    times.dedup();
    if !same || times.len() != n / d {
        return None;
    }
    Some(DateGroups {
        // the date's text as the name had it is parsed again
        dates: (0..n / d).map(|g| crate::dates::format_date(parts[g * d].0.0, parts[g * d].0.1)).collect(),
        bands: d,
        names: parts[..d].iter().map(|p| p.1.clone()).collect(),
        source: String::new(),
    })
}

/// `<name>_dates.csv` (or .txt, `.dates`, `-dates`, in any case) next to the file: the file and its dates, a row
/// each, from its column named Date (else the first column whose every row is a date); a header row or none,
/// separated by commas, semicolons, tabs or spaces.
fn sidecar(path: &str) -> Option<(PathBuf, Vec<String>)> {
    let p = Path::new(path);
    let stem = p.file_stem()?.to_string_lossy().to_lowercase();
    let dir = match p.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    let wanted: Vec<String> =
        ["_dates", ".dates", "-dates"].iter().flat_map(|s| ["csv", "txt"].map(|e| format!("{stem}{s}.{e}"))).collect();
    let mut found: Vec<(usize, PathBuf)> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let k = wanted.iter().position(|w| *w == e.file_name().to_string_lossy().to_lowercase())?;
            e.metadata().is_ok_and(|m| m.is_file() && m.len() < 8 << 20).then(|| (k, e.path()))
        })
        .collect();
    found.sort();
    let file = found.into_iter().next()?.1;
    let text = std::fs::read_to_string(&file).ok()?;
    let dates = dates_of_table(&text)?;
    Some((file, dates))
}

/// The dates of a table's rows.
fn dates_of_table(text: &str) -> Option<Vec<String>> {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let first = lines.first()?;
    let sep: Option<char> = [',', ';', '\t'].into_iter().find(|&c| first.contains(c));
    let cells = |l: &str| -> Vec<String> {
        match sep {
            Some(c) => l.split(c).map(|s| s.trim().trim_matches('"').to_string()).collect(),
            None => l.split_whitespace().map(|s| s.trim_matches('"').to_string()).collect(),
        }
    };
    let rows: Vec<Vec<String>> = lines.iter().map(|l| cells(l)).collect();
    let header = rows[0].iter().all(|c| parse_date(c).is_none());
    let body = &rows[header as usize..];
    let column = |k: usize| -> Option<Vec<String>> {
        body.iter().map(|r| r.get(k).filter(|c| parse_date(c).is_some()).cloned()).collect()
    };
    let width = rows[0].len();
    let named = header.then(|| {
        let names: Vec<String> = rows[0].iter().map(|c| c.to_lowercase()).collect();
        names.iter().position(|c| c == "date").or_else(|| names.iter().position(|c| c.contains("date")))
    });
    named.flatten().and_then(column).or_else(|| (0..width).find_map(column)).filter(|d| !d.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables() {
        let cdts = "Date,Fractional_Year,Ordinal_Day\n2020-02-04,2020.09,737459\n2020-05-26,2020.39,737571\n";
        assert_eq!(dates_of_table(cdts).unwrap(), ["2020-02-04", "2020-05-26"]);
        // no header, a date a line
        assert_eq!(dates_of_table("20200204\n20200526\n").unwrap(), ["20200204", "20200526"]);
        // the date column not first, by its name
        assert_eq!(dates_of_table("id;acquired\n1;2001-03-04\n2;2001-03-20").unwrap(), ["2001-03-04", "2001-03-20"]);
        assert!(dates_of_table("a,b\nx,y\n").is_none());
    }

    #[test]
    fn names_of_dates_and_bands() {
        let split = |names: &[&str]| names.iter().map(|n| split_date(n)).collect::<Vec<_>>();
        let g = by_names(&split(&["2020-01-15_red", "2020-01-15_nir", "2020-02-16_red", "2020-02-16_nir"])).unwrap();
        assert_eq!(
            (g.dates, g.bands, g.names),
            (vec!["2020-01-15".to_string(), "2020-02-16".into()], 2, vec!["red".to_string(), "nir".into()])
        );
        // bands in another order on a date, a date twice, a band without its date: not groups
        assert!(by_names(&split(&["2020-01-15_red", "2020-01-15_nir", "2020-02-16_nir", "2020-02-16_red"])).is_none());
        assert!(by_names(&split(&["2020-01-15_red", "2020-01-15_nir", "2020-01-15_red", "2020-01-15_nir"])).is_none());
        let twice = [
            "2020-01-15_red",
            "2020-01-15_nir",
            "2020-02-16_red",
            "2020-02-16_nir",
            "2020-01-15_red",
            "2020-01-15_nir",
        ];
        assert!(by_names(&split(&twice)).is_none());
        assert!(by_names(&split(&["2020-01-15_red", "2020-01-15_nir", "red", "nir"])).is_none());
    }
}
