//! Dates: days since 1970-01-01, the dates in file and band names, and their text (src/cube.cpp).

use std::sync::LazyLock;

use regex::Regex;

/// How precise the dates of a series are; also the axis' labels. Ordered: a series takes the finest of its layers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum TimeGranularity {
    #[default]
    Index,
    Year,
    Month,
    Day,
    Hour,
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's algorithm).
pub fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = i64::from(y) - i64::from(m <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + i64::from(doe) - 719468
}

/// The date (year, month, day) of a day since 1970-01-01.
pub fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    ((i64::from(yoe) + era * 400 + i64::from(m <= 2)) as i32, m, d)
}

pub fn is_leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn day_seconds(days: i64) -> f64 {
    days as f64 * 86400.0
}

static YMD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|[^0-9])((?:19|20)[0-9]{2})[-_.]?(0[1-9]|1[0-2])[-_.]?(0[1-9]|[12][0-9]|3[01])(?:[^0-9]|$)")
        .unwrap()
});
// MODIS: A2001001
static YDOY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"A((?:19|20)[0-9]{2})([0-9]{3})(?:[^0-9]|$)").unwrap());
static YM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[^0-9])((?:19|20)[0-9]{2})[-_.](0[1-9]|1[0-2])(?:[^0-9]|$)").unwrap());
static Y: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:^|[^0-9])((?:19|20)[0-9]{2})(?:[^0-9]|$)").unwrap());

/// A date in the text: its Unix seconds and how precise it is (`None` without one).
pub fn parse_date(s: &str) -> Option<(f64, TimeGranularity)> {
    let num = |c: &regex::Captures, i: usize| c[i].parse::<u32>().unwrap();
    if let Some(c) = YMD.captures(s) {
        let days = days_from_civil(num(&c, 1) as i32, num(&c, 2), num(&c, 3));
        return Some((day_seconds(days), TimeGranularity::Day));
    }
    if let Some(c) = YDOY.captures(s) {
        let doy = num(&c, 2);
        if (1..=366).contains(&doy) {
            let days = days_from_civil(num(&c, 1) as i32, 1, 1) + i64::from(doy) - 1;
            return Some((day_seconds(days), TimeGranularity::Day));
        }
    }
    if let Some(c) = YM.captures(s) {
        let days = days_from_civil(num(&c, 1) as i32, num(&c, 2), 1);
        return Some((day_seconds(days), TimeGranularity::Month));
    }
    if let Some(c) = Y.captures(s) {
        return Some((day_seconds(days_from_civil(num(&c, 1) as i32, 1, 1)), TimeGranularity::Year));
    }
    None
}

/// Labels as precise as the dates need: years, months, days or hours.
pub fn granularity_of(times: impl IntoIterator<Item = f64>) -> TimeGranularity {
    let (mut jan1, mut first, mut midnight) = (true, true, true);
    for t in times {
        let day = (t / 86400.0).floor() as i64;
        let (_, m, d) = civil_from_days(day);
        midnight = midnight && (t - day as f64 * 86400.0).abs() < 1.0;
        first = first && d == 1;
        jan1 = jan1 && d == 1 && m == 1;
    }
    if !midnight {
        TimeGranularity::Hour
    } else if jan1 {
        TimeGranularity::Year
    } else if first {
        TimeGranularity::Month
    } else {
        TimeGranularity::Day
    }
}

/// The text of Unix seconds at a granularity ("2003", "2003-07", "2003-07-14", "2003-07-14 10:30").
pub fn format_date(t: f64, granularity: TimeGranularity) -> String {
    let days = (t / 86400.0).floor() as i64;
    let (y, m, d) = civil_from_days(days);
    match granularity {
        TimeGranularity::Year => format!("{y:04}"),
        TimeGranularity::Month => format!("{y:04}-{m:02}"),
        TimeGranularity::Hour => {
            let s = t.floor() as i64 - days * 86400;
            format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", s / 3600, s / 60 % 60)
        }
        _ => format!("{y:04}-{m:02}-{d:02}"),
    }
}

/// Decimal year of Unix seconds (e.g. 2003.5).
pub fn decimal_year(t: f64) -> f64 {
    let days = (t / 86400.0).floor() as i64;
    let (y, _, _) = civil_from_days(days);
    f64::from(y) + (days - days_from_civil(y, 1, 1)) as f64 / if is_leap(y) { 366.0 } else { 365.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trip() {
        for z in -800_000..800_000 {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11017);
    }

    #[test]
    fn dates_in_names() {
        let day = |y, m, d| days_from_civil(y, m, d) as f64 * 86400.0;
        assert_eq!(parse_date("landsat_medoid_1985"), Some((day(1985, 1, 1), TimeGranularity::Year)));
        assert_eq!(parse_date("S2_20230811_NDVI"), Some((day(2023, 8, 11), TimeGranularity::Day)));
        assert_eq!(parse_date("ndvi_2003-07"), Some((day(2003, 7, 1), TimeGranularity::Month)));
        assert_eq!(parse_date("MOD13Q1.A2001033.h12v10"), Some((day(2001, 2, 2), TimeGranularity::Day)));
        assert_eq!(parse_date("band 7"), None);
        // a number that only contains a year is not one
        assert_eq!(parse_date("tile_120056"), None);
    }

    #[test]
    fn labels() {
        let t = days_from_civil(2003, 7, 14) as f64 * 86400.0;
        assert_eq!(format_date(t, TimeGranularity::Year), "2003");
        assert_eq!(format_date(t, TimeGranularity::Day), "2003-07-14");
        assert_eq!(format_date(t + 37800.0, TimeGranularity::Hour), "2003-07-14 10:30");
        assert_eq!(granularity_of([0.0, day_seconds(365)]), TimeGranularity::Year);
        assert!((decimal_year(day_seconds(days_from_civil(2004, 7, 2))) - (2004.0 + 183.0 / 366.0)).abs() < 1e-12);
    }
}
