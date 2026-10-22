//! What Zeit 2.0 still does in Python and Janus needs, written to move into zeit-core as it is (RUST_MIGRATION.md:
//! "a lógica de algoritmo vai para o zeit-core"). Each function says the Python it reproduces, bit for bit.

/// How `extract_events` picks a pixel's event among its segments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortBy {
    Greatest = 1,
    Newest = 2,
    Fastest = 3,
    Longest = 4,
    Dsnr = 5,
}

impl SortBy {
    pub fn parse(s: &str) -> Option<SortBy> {
        Some(match s.to_ascii_lowercase().as_str() {
            "greatest" => SortBy::Greatest,
            "newest" => SortBy::Newest,
            "fastest" => SortBy::Fastest,
            "longest" => SortBy::Longest,
            "dsnr" => SortBy::Dsnr,
            _ => return None,
        })
    }
}

/// One pixel's event (zeros: none), in the kernel's types.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Event {
    pub yod: u16,
    pub magnitude: f32,
    pub duration: u16,
    pub pre_val: f32,
    pub post_val: f32,
    pub rate: f32,
    pub dsnr: f32,
}

/// zeit/metrics.py `_extract_py` on one pixel: the event of LandTrendr's vertices (years, then values; 0 past
/// the last vertex, the values' NaN as 0) a pixel keeps. In float32 as the numba kernel, the thresholds in
/// float64.
pub fn landtrendr_event(
    years: &[f32],
    values: &[f32],
    rmse: f32,
    is_loss: bool,
    sort_by: SortBy,
    min_mag: f64,
    min_dur: f64,
    pre_thresh: f64,
) -> Event {
    let mut best_score = -999999.0f64;
    let mut out = Event::default();
    let max_v = years.len();
    for i in 0..max_v.saturating_sub(1) {
        let (start_year, end_year) = (years[i], years[i + 1]);
        if start_year == 0.0 || end_year == 0.0 {
            break; // no more valid vertices
        }
        let duration = end_year - start_year;
        if duration <= 0.0 {
            continue;
        }
        let (pre_val, post_val) = (values[i], values[i + 1]);
        let magnitude = if is_loss {
            if pre_thresh > 0.0 && f64::from(pre_val) < pre_thresh {
                continue;
            }
            pre_val - post_val
        } else {
            if pre_thresh > 0.0 && f64::from(pre_val) > pre_thresh {
                continue;
            }
            post_val - pre_val
        };
        if magnitude <= 0.0 || f64::from(magnitude) < min_mag {
            continue; // a flat segment is no event
        }
        if f64::from(duration) < min_dur {
            continue;
        }
        let rate = magnitude / duration;
        let dsnr = if rmse > 0.0 { magnitude / rmse } else { 0.0 };
        let score = f64::from(match sort_by {
            SortBy::Greatest => magnitude,
            SortBy::Newest => start_year,
            SortBy::Fastest => rate,
            SortBy::Longest => duration,
            SortBy::Dsnr => dsnr,
        });
        if score > best_score {
            best_score = score;
            out = Event {
                yod: start_year as u16,
                magnitude,
                duration: duration as u16,
                pre_val,
                post_val,
                rate,
                dsnr,
            };
        }
    }
    out
}
