//! Phenology (zeit_bridge/tool_phenology.py): zeit.phenology per season (zeit-core's port of phenofit) on the
//! gap-filled series; the seasons' start, peak and end drawn and listed in Zeit's day numbers (days since January
//! 1st of the first year, 1-indexed), as the bridge converted them.

use zeit_core::phenology::{Settings, fit_phenology_batch};

use crate::numeric::{civil, is_leap, median, timedelta_days, year_start};
use crate::pixel::{Overlay, Params, PixelInput, PixelResult, g, row};
use crate::upstream::np_interp;

/// zeit.phenology's metrics, in its order.
const METRICS: [&str; 21] = [
    "TRS2.sos",
    "TRS2.eos",
    "TRS5.sos",
    "TRS5.eos",
    "TRS6.sos",
    "TRS6.eos",
    "DER.sos",
    "DER.pos",
    "DER.eos",
    "UD",
    "SD",
    "DD",
    "RD",
    "Greenup",
    "Maturity",
    "Senescence",
    "Dormancy",
    "LOS",
    "POP",
    "R2",
    "RMSE",
];

fn metric(name: &str) -> usize {
    METRICS.iter().position(|m| *m == name).unwrap_or(0)
}

/// The start and end of season of each extraction method.
fn method_metrics(method: &str) -> Option<(&'static str, &'static str)> {
    Some(match method {
        "trs5" => ("TRS5.sos", "TRS5.eos"),
        "trs2" => ("TRS2.sos", "TRS2.eos"),
        "trs6" => ("TRS6.sos", "TRS6.eos"),
        "der" => ("DER.sos", "DER.eos"),
        "zhang" => ("Greenup", "Dormancy"),
        "gu" => ("UD", "RD"),
        _ => return None,
    })
}

fn curve_code(curve: &str) -> Option<i64> {
    Some(match curve {
        "beck" => 0,
        "elmore" => 1,
        "gu" => 2,
        "klos" => 3,
        "zhang" => 4,
        "ag" => 5,
        "dl" => 6,
        _ => return None,
    })
}

fn diy(y: i64) -> f64 {
    if is_leap(y) { 366.0 } else { 365.0 }
}

/// Decimal years -> days since base-01-01, 1-indexed (the bridge's `_to_days`).
fn to_days(years: &[f64], base: i64) -> Vec<f64> {
    years
        .iter()
        .map(|&yr| {
            let y = yr.floor() as i64;
            (year_start(y) - year_start(base)) as f64 + (yr - y as f64) * diy(y) + 1.0
        })
        .collect()
}

/// The date of a day number: days since 1970.
fn day_date(d: f64, base: i64) -> i64 {
    year_start(base) + timedelta_days(d - 1.0)
}

/// A day number as a decimal year (the bridge's `_days_to_decimal_year`).
fn days_to_decimal_year(d: f64, base: i64) -> f64 {
    let date = day_date(d, base);
    let (y, ..) = civil(date);
    let yday = date - year_start(y) + 1;
    let frac = d - d.floor();
    y as f64 + ((yday - 1) as f64 + frac) / diy(y)
}

fn fmt_day(d: f64, base: i64) -> String {
    let (y, m, dd) = civil(day_date(d, base));
    format!("{y:04}-{m:02}-{dd:02}")
}

/// A value as Python's f"{v:.Nf}".
fn fixed(v: f64, n: usize) -> String {
    if v.is_nan() { "nan".into() } else { format!("{v:.n$}") }
}

fn nanmedian(v: &[f64]) -> f64 {
    let ok: Vec<f64> = v.iter().copied().filter(|x| !x.is_nan()).collect();
    median(&ok)
}

/// A pixel's season (in Zeit's day numbers), with the peak and amplitude of its observations.
struct Season {
    sos: f64,
    pos: f64,
    eos: f64,
    los: f64,
    peak: f64,
    amp: f64,
    r2: f64,
}

/// zeit.phenology per season of every pixel of `values` (pixels x T, NaN missing), as the bridge's `_fit` and
/// `_season_table`: the gap-filled series (pixels x T) and each pixel's fitted seasons.
fn fit(
    p: &Params,
    ctx: &PixelInput,
    values: &[f64],
    n: usize,
    n_jobs: i32,
) -> Result<(Vec<f64>, Vec<Vec<Season>>), String> {
    let years = &ctx.years;
    let t = years.len();
    let base = years[0].floor() as i64;
    let days = to_days(years, base);
    let span = (years[t - 1] - years[0]).max(1.0);
    let s_max = (2.0 * span.ceil() + 2.0).min(200.0) as usize;

    // gaps filled linearly by index, with weight 0 (the bridge's `_prepare`)
    let idx: Vec<f64> = (0..t).map(|i| i as f64).collect();
    let mut v = values.to_vec();
    let w: Vec<f64> = values.iter().map(|x| f64::from(u8::from(x.is_finite()))).collect();
    let mut has_data = vec![false; n];
    for px in 0..n {
        let row = &mut v[px * t..(px + 1) * t];
        let (gx, gy): (Vec<f64>, Vec<f64>) = (0..t).filter(|&i| row[i].is_finite()).map(|i| (i as f64, row[i])).unzip();
        has_data[px] = !gx.is_empty();
        if has_data[px] && gx.len() < t {
            for i in 0..t {
                if !row[i].is_finite() {
                    row[i] = np_interp(idx[i], &gx, &gy);
                }
            }
        }
    }

    // zeit.phenology(annual=False): its own day numbers, from the dates
    let date_days = ctx.days.as_ref().ok_or("ValueError: the time axis has no dates: pass dates=")?;
    let base_year = date_days.iter().map(|&d| civil(d).0).min().unwrap_or(1970);
    let day_numbers: Vec<f64> = date_days.iter().map(|&d| (d - year_start(base_year) + 1) as f64).collect();
    let curve = p.str("curve");
    let smoothing = p.str("smoothing");
    let settings = Settings {
        curve_type: curve_code(curve).ok_or_else(|| format!("ValueError: curve must be one of ..., got {curve:?}"))?,
        max_seasons: s_max,
        whittaker_lambda: p.f64("lambda"),
        apply_whittaker: smoothing == "whittaker",
        apply_hants: smoothing == "hants",
        hants_frequencies: p.int("hants_frequencies") as i32,
        hants_threshold: 0.1,
        min_season_length: p.int("min_season_length") as i32,
        min_amplitude: p.f64("min_amplitude"),
        min_pixel_amplitude: p.f64("min_pixel_amplitude"),
        rtrough_max: 0.6,
        r_min_filter: 0.02,
        season_retry: true,
    };
    // (metric, pixel, season) as float32
    let out: Vec<f64> = fit_phenology_batch(&v, &day_numbers, n, Some(&w), &settings, n_jobs)
        .into_iter()
        .map(|x| f64::from(x as f32))
        .collect();
    let (k_sos, k_eos) = method_metrics(p.str("method")).ok_or("KeyError: method")?;
    let seasons = (0..n)
        .map(|px| {
            if !has_data[px] {
                return vec![];
            }
            let m = |name: &str, s: usize| out[(metric(name) * n + px) * s_max + s];
            // peak and amplitude from the observations (the bridge's `_season_table`)
            let obs: Vec<f64> = (0..t).map(|i| if w[px * t + i] > 0.0 { v[px * t + i] } else { f64::NAN }).collect();
            let fold = |pick: &dyn Fn(usize) -> bool, max: bool| {
                (0..t)
                    .filter(|&i| pick(i) && !obs[i].is_nan())
                    .map(|i| obs[i])
                    .fold(f64::NAN, |a, x| if a.is_nan() || (max && x > a) || (!max && x < a) { x } else { a })
            };
            (0..s_max)
                .filter(|&s| m("POP", s).is_finite())
                .map(|s| {
                    let (sos, pos, eos) = (m(k_sos, s), m("POP", s), m(k_eos, s));
                    let peak = fold(&|i| days[i] >= sos && days[i] <= eos, true);
                    let low = fold(&|i| (days[i] - pos).abs() <= 182.6, false);
                    Season { sos, pos, eos, los: eos - sos, peak, amp: peak - low, r2: m("R2", s) }
                })
                .collect()
        })
        .collect();
    Ok((v, seasons))
}

/// The calendar year of a season's peak (a day number).
fn pos_year(pos: f64, base: i64) -> i64 {
    civil(year_start(base) + (pos.floor() as i64 - 1)).0
}

pub fn pixel(p: &Params, input: &PixelInput) -> Result<PixelResult, String> {
    let years = &input.years;
    let base = years[0].floor() as i64;
    let days = to_days(years, base);
    let vals = &input.values;
    let n_valid = vals.iter().filter(|v| v.is_finite()).count();
    if n_valid < 6 {
        return Ok(PixelResult {
            overlays: vec![],
            rows: vec![row("Phenology", format!("not enough data ({n_valid} valid)"))],
        });
    }
    if years[years.len() - 1] - years[0] < 1.0 {
        return Ok(PixelResult { overlays: vec![], rows: vec![row("Phenology", "needs at least one year of data")] });
    }
    let (v, mut all) = fit(p, input, vals, 1, 1)?;
    let seasons = all.remove(0);
    if seasons.is_empty() {
        let finite = vals.iter().copied().filter(|x| !x.is_nan());
        let range = finite.clone().fold(f64::NEG_INFINITY, f64::max) - finite.fold(f64::INFINITY, f64::min);
        let mpa = p.f64("min_pixel_amplitude");
        let why = if range < mpa {
            format!("pixel amplitude {} < {}", g(range, 4), g(mpa, 6))
        } else {
            "no growing season found".into()
        };
        return Ok(PixelResult { overlays: vec![], rows: vec![row("Phenology", why)] });
    }

    let (mut lines, mut px, mut py) = (vec![], vec![], vec![]);
    let mut rows = vec![row("Seasons", seasons.len().to_string())];
    for s in &seasons {
        for d in [s.sos, s.eos] {
            if d.is_finite() {
                lines.push(days_to_decimal_year(d, base));
            }
        }
        if s.pos.is_finite() {
            px.push(days_to_decimal_year(s.pos, base));
            py.push(Some(np_interp(s.pos, &days, &v)));
        }
    }
    let shown = &seasons[seasons.len().saturating_sub(12)..];
    if shown.len() < seasons.len() {
        rows.push(row(format!("  (latest {} shown)", shown.len()), ""));
    }
    for s in shown {
        let label = if s.pos.is_finite() { format!("Peak {}", fmt_day(s.pos, base)) } else { "Season".into() };
        let mut parts = vec![];
        if s.sos.is_finite() && s.eos.is_finite() {
            parts.push(format!("{} to {}", fmt_day(s.sos, base), fmt_day(s.eos, base)));
        }
        if s.los.is_finite() {
            parts.push(format!("{} d", fixed(s.los, 0)));
        }
        if s.peak.is_finite() {
            parts.push(format!("peak {}", g(s.peak, 4)));
        }
        if s.r2.is_finite() {
            parts.push(format!("R2 {}", fixed(s.r2, 2)));
        }
        rows.push(row(label, if parts.is_empty() { "-".into() } else { parts.join(", ") }));
    }
    // the typical season: medians of the days of the year of the seasons' peaks
    let rel = |f: fn(&Season) -> f64| -> Vec<f64> {
        seasons
            .iter()
            .map(|s| {
                let jan1 = (year_start(pos_year(s.pos, base)) - year_start(base)) as f64 + 1.0;
                f(s) - jan1 + 1.0
            })
            .collect()
    };
    let med = [nanmedian(&rel(|s| s.sos)), nanmedian(&rel(|s| s.pos)), nanmedian(&rel(|s| s.eos))];
    rows.push(row(
        "Median SOS / POS / EOS (DOY)",
        format!("{} / {} / {}", fixed(med[0], 0), fixed(med[1], 0), fixed(med[2], 0)),
    ));
    let los: Vec<f64> = seasons.iter().map(|s| s.los).collect();
    rows.push(row("Median length", format!("{} days", fixed(nanmedian(&los), 0))));

    let mut overlays = vec![];
    if !lines.is_empty() {
        overlays.push(Overlay::new("vlines", "Season start/end", lines, vec![]));
    }
    if !px.is_empty() {
        overlays.push(Overlay::new("markers", "Season peak", px, py));
    }
    Ok(PixelResult { overlays, rows })
}

pub fn chunk(p: &Params, chunk: &crate::job::Chunk, ctx: &crate::job::JobCtx) -> Result<crate::job::ChunkOut, String> {
    let base = ctx.input.years[0].floor() as i64;
    let values = chunk.by_pixel(&chunk.stack);
    let (t, n) = (chunk.t, chunk.pixels());
    let (_, all) = fit(p, &ctx.input, &values, n, ctx.n_jobs)?;
    let names = ["sos", "pos", "eos", "los", "peak", "amplitude", "r2"];
    let mut res: crate::job::ChunkOut = names.iter().map(|k| (k.to_string(), Vec::with_capacity(n))).collect();
    let mut n_seasons = Vec::with_capacity(n);
    let mode = p.str("season");
    let year = p.int("year");
    for (px, seasons) in all.iter().enumerate() {
        // the dates as days of the year of the season's peak; the rest as they are
        let per = |s: &Season| -> [f64; 7] {
            let jan1 = (year_start(pos_year(s.pos, base)) - year_start(base)) as f64 + 1.0;
            [s.sos - jan1 + 1.0, s.pos - jan1 + 1.0, s.eos - jan1 + 1.0, s.los, s.peak, s.amp, s.r2]
        };
        let rows: Vec<[f64; 7]> = seasons.iter().map(per).collect();
        let picked: [f64; 7] = if mode == "median" {
            std::array::from_fn(|k| nanmedian(&rows.iter().map(|r| r[k]).collect::<Vec<_>>()))
        } else {
            let sel = |s: &Season| !(mode == "year" && year > 0) || pos_year(s.pos, base) == year;
            match seasons.iter().rposition(sel) {
                Some(i) => rows[i],
                None => [f64::NAN; 7],
            }
        };
        for (k, name) in names.iter().enumerate() {
            res.get_mut(*name).unwrap().push(picked[k]);
        }
        let any = (0..t).any(|i| values[px * t + i].is_finite());
        n_seasons.push(if any { seasons.len() as f64 } else { f64::NAN });
    }
    res.insert("n_seasons".into(), n_seasons);
    Ok(res)
}
