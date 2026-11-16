//! The response design "one label per year" of the Interpretation panel (Validation Viewer's second mode, by Dr.
//! Matthias Baumann, Humboldt-Universität zu Berlin, GPL-2.0-or-later, used under GPL-3.0-or-later): the pure parts,
//! which years a cube has, the date that stands for a year, the year to label next, a point's one-line summary and
//! the tables the labels are exported as.

use janus_validation::{Label, YEAR_NA, year_of};

/// The years of a list of dates (as the series shows them), sorted, one each.
pub fn cube_years(dates: &[String]) -> Vec<i32> {
    let mut ys: Vec<i32> = dates.iter().map(|d| year_of(d)).filter(|&y| y != 0).collect();
    ys.sort_unstable();
    ys.dedup();
    ys
}

/// The index of the date nearest to `year`: the first of that year, else of the nearest year (0 = unknown, skipped).
pub fn nearest_date(date_years: &[i32], year: i32) -> Option<usize> {
    date_years.iter().enumerate().filter(|&(_, &y)| y != 0).min_by_key(|&(i, &y)| ((y - year).abs(), i)).map(|(i, _)| i)
}

/// The year after (`dir` 1) or before (-1) `current` in `years`, round the ends; the first (last) without one.
pub fn step_year(years: &[i32], current: Option<i32>, dir: i32) -> Option<i32> {
    if years.is_empty() {
        return None;
    }
    let n = years.len() as i32;
    match current.and_then(|c| years.iter().position(|&y| y == c)) {
        Some(p) => Some(years[(p as i32 + dir).rem_euclid(n) as usize]),
        None => Some(if dir >= 0 { years[0] } else { years[years.len() - 1] }),
    }
}

/// The first year of `years` without a label, else the first year.
pub fn first_open_year(l: &Label, years: &[i32]) -> Option<i32> {
    years.iter().copied().find(|y| l.years.get(y).is_none_or(|x| x.label.is_empty())).or_else(|| years.first().copied())
}

/// A point's answer in a line: the classes in order of the years, each run once, and the years each one begins
/// ("Forest→Deforestation 2020"); "n/a" years skipped; how many years are labelled while some are not.
pub fn summary(l: &Label, years: &[i32]) -> String {
    let mut runs: Vec<(&str, i32)> = Vec::new();
    for &y in years {
        let Some(x) = l.years.get(&y).filter(|x| !x.label.is_empty() && x.label != YEAR_NA) else { continue };
        if runs.last().is_none_or(|r| r.0 != x.label) {
            runs.push((&x.label, y));
        }
    }
    let given = l.years_given(years);
    let mut s = match runs.len() {
        0 if given > 0 => YEAR_NA.to_string(),
        0 => String::new(),
        1 => runs[0].0.to_string(),
        _ => {
            let classes: Vec<&str> = runs.iter().map(|r| r.0).collect();
            let at: Vec<String> = runs[1..].iter().map(|r| r.1.to_string()).collect();
            format!("{} {}", classes.join("→"), at.join(", "))
        }
    };
    if given > 0 && given < years.len() {
        s += &format!(" ({given}/{})", years.len());
    }
    s
}

/// The names of the columns of the labels by year of a table: v_class_<year>, v_conf_<year>, for each year.
pub fn year_columns(years: &[i32]) -> Vec<String> {
    years.iter().flat_map(|y| [format!("v_class_{y}"), format!("v_conf_{y}")]).collect()
}

/// The value of column k of `year_columns(years)` for a label ("" when the year has none).
pub fn year_value(l: &Label, years: &[i32], k: usize) -> String {
    let Some(x) = years.get(k / 2).and_then(|y| l.years.get(y)).filter(|x| !x.label.is_empty()) else {
        return String::new();
    };
    if k.is_multiple_of(2) {
        x.label.clone()
    } else if x.confidence > 0 {
        x.confidence.to_string()
    } else {
        String::new()
    }
}

/// A point of `by_year_csv`: its id, where it is (lon, lat), whose labels, and the labels.
pub struct YearPoint<'a> {
    pub pid: String,
    pub lon_lat: Option<(f64, f64)>,
    pub interpreter: &'a str,
    pub label: &'a Label,
}

/// One row for each point and year: pid, lon, lat, year, label, confidence, year_done (this year has a label),
/// point_done (all of them have), interpreter.
pub fn by_year_csv(points: &[YearPoint], years: &[i32], csv_field: &dyn Fn(&str) -> String) -> String {
    let mut out = String::from("pid,lon,lat,year,label,confidence,year_done,point_done,interpreter\n");
    for p in points {
        let done = p.label.years_done(years);
        for &y in years {
            let x = p.label.years.get(&y).filter(|x| !x.label.is_empty());
            let (lon, lat) =
                p.lon_lat.map_or((String::new(), String::new()), |(a, b)| (format!("{a:.8}"), format!("{b:.8}")));
            out += &format!(
                "{},{lon},{lat},{y},{},{},{},{},{}\n",
                csv_field(&p.pid),
                csv_field(x.map_or("", |x| x.label.as_str())),
                x.filter(|x| x.confidence > 0).map_or(String::new(), |x| x.confidence.to_string()),
                i32::from(x.is_some()),
                i32::from(done),
                csv_field(p.interpreter)
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use janus_validation::YearLabel;

    fn by_year(v: &[(i32, &str, i32)]) -> Label {
        Label {
            years: v
                .iter()
                .map(|&(y, l, c)| (y, YearLabel { label: l.into(), confidence: c, updated: String::new() }))
                .collect(),
            ..Default::default()
        }
    }

    fn field(s: &str) -> String {
        s.to_string()
    }

    #[test]
    fn dates_and_years() {
        let dates: Vec<String> = ["2005-06-01", "2005-09-10", "2015-07-14", "2019", "x"].map(String::from).to_vec();
        assert_eq!(cube_years(&dates), vec![2005, 2015, 2019]);
        let ys: Vec<i32> = dates.iter().map(|d| year_of(d)).collect();
        assert_eq!(nearest_date(&ys, 2005), Some(0));
        assert_eq!(nearest_date(&ys, 2014), Some(2));
        assert_eq!(nearest_date(&ys, 2030), Some(3));
        assert_eq!(nearest_date(&[0, 0], 2010), None);
        let years = [2005, 2015, 2019];
        assert_eq!(step_year(&years, Some(2005), 1), Some(2015));
        assert_eq!(step_year(&years, Some(2019), 1), Some(2005));
        assert_eq!(step_year(&years, Some(2005), -1), Some(2019));
        assert_eq!(step_year(&years, None, 1), Some(2005));
        assert_eq!(step_year(&years, None, -1), Some(2019));
        assert_eq!(step_year(&[], Some(1), 1), None);
        let l = by_year(&[(2005, "Forest", 3), (2015, YEAR_NA, 0)]);
        assert_eq!(first_open_year(&l, &years), Some(2019));
        assert_eq!(first_open_year(&by_year(&[(2005, "A", 1), (2015, "A", 1), (2019, "A", 1)]), &years), Some(2005));
    }

    #[test]
    fn one_line_of_years() {
        let ys = [2005, 2015, 2019, 2023];
        let l = by_year(&[(2005, "Forest", 3), (2015, "Forest", 0), (2019, "Deforestation", 2), (2023, "Pasture", 1)]);
        assert_eq!(summary(&l, &ys), "Forest→Deforestation→Pasture 2019, 2023");
        let l = by_year(&[(2005, "Forest", 3), (2015, "Forest", 0), (2019, YEAR_NA, 0), (2023, "Deforestation", 1)]);
        assert_eq!(summary(&l, &ys), "Forest→Deforestation 2023");
        assert_eq!(summary(&by_year(&[(2005, "Forest", 1)]), &ys), "Forest (1/4)");
        assert_eq!(summary(&by_year(&[(2005, YEAR_NA, 0), (2015, YEAR_NA, 0)]), &ys[..2]), "n/a");
        assert_eq!(summary(&Label::default(), &ys), "");
    }

    #[test]
    fn tables_by_year() {
        let ys = [2018, 2019];
        let l = by_year(&[(2018, "Forest", 3), (2019, "Fire, big", 0)]);
        assert_eq!(year_columns(&ys), vec!["v_class_2018", "v_conf_2018", "v_class_2019", "v_conf_2019"]);
        let v: Vec<String> = (0..4).map(|k| year_value(&l, &ys, k)).collect();
        assert_eq!(v, vec!["Forest", "3", "Fire, big", ""]);
        let part = by_year(&[(2018, YEAR_NA, 0)]);
        let pts = [
            YearPoint { pid: "7".into(), lon_lat: Some((-55.5, -12.25)), interpreter: "ana", label: &l },
            YearPoint { pid: "8".into(), lon_lat: None, interpreter: "bruno", label: &part },
        ];
        let csv = by_year_csv(&pts, &ys, &|s| if s.contains(',') { format!("\"{s}\"") } else { field(s) });
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines[0], "pid,lon,lat,year,label,confidence,year_done,point_done,interpreter");
        assert_eq!(lines[1], "7,-55.50000000,-12.25000000,2018,Forest,3,1,1,ana");
        assert_eq!(lines[2], "7,-55.50000000,-12.25000000,2019,\"Fire, big\",,1,1,ana");
        assert_eq!(lines[3], "8,,,2018,n/a,,1,0,bruno");
        assert_eq!(lines[4], "8,,,2019,,,0,0,bruno");
    }
}
