//! The time series chart's second Y axis (phase 61): a layer's series on the right axis, drawn as bars or as a
//! line, and shifted by the lag of a `corr(x, y, lag=k)` of the calculator. egui_plot has one Y scale, so a series
//! of the right axis is drawn through an affine map into the left axis' range (`y' = a * y + b`) and the right
//! axis' marks are the left's, labelled with the inverse map. The two axes are fitted together: both get the same
//! number of intervals, each on round steps (1, 2 or 5 times a power of ten), so that a mark of one is a mark of the
//! other and the grid serves both.

use std::collections::HashMap;

use janus_cube::dates::{civil_from_days, days_from_civil};

const DAY: f64 = 86400.0;

/// An affine map `y' = a * y + b` (a > 0): a value of the right axis into the left axis' scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine {
    pub a: f64,
    pub b: f64,
}

impl Affine {
    pub const IDENTITY: Affine = Affine { a: 1.0, b: 0.0 };

    pub fn fwd(self, y: f64) -> f64 {
        self.a * y + self.b
    }

    pub fn inv(self, y: f64) -> f64 {
        (y - self.b) / self.a
    }
}

/// One axis' marks: `start + i * step` for i in 0..=n.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Marks {
    pub start: f64,
    pub step: f64,
    pub n: usize,
}

impl Marks {
    /// The marks' values (a zero is exactly 0).
    pub fn values(&self) -> Vec<f64> {
        (0..=self.n)
            .map(|i| {
                let v = self.start + i as f64 * self.step;
                if v.abs() < self.step * 1e-9 { 0.0 } else { v }
            })
            .collect()
    }

    pub fn end(&self) -> f64 {
        self.start + self.n as f64 * self.step
    }

    /// A mark's text at the step's decimals (a zero without its sign).
    pub fn label(&self, v: f64) -> String {
        let text = format!("{v:.*}", self.decimals());
        if text.trim_start_matches('-').chars().all(|c| c == '0' || c == '.') { text.replace('-', "") } else { text }
    }

    /// The decimals a mark's text needs (0.2 -> 1, 5 -> 0, 50 -> 0).
    pub fn decimals(&self) -> usize {
        (-self.step.log10().floor()).max(0.0) as usize
    }
}

/// The two axes fitted together.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DualAxes {
    pub left: Marks,
    pub right: Marks,
    /// the right axis' values into the left's scale
    pub map: Affine,
}

impl DualAxes {
    /// The right axis' text of a mark at `y` of the left scale: the inverse map, to the step's decimals.
    pub fn right_label(&self, y: f64) -> String {
        self.right.label(self.map.inv(y))
    }
}

/// The smallest round step (1, 2 or 5 times a power of ten) of at least `x`.
fn nice_at_least(x: f64) -> f64 {
    let e = x.log10().floor();
    let m = x / 10f64.powf(e);
    let m = [1.0, 2.0, 5.0, 10.0].into_iter().find(|&c| c >= m * (1.0 - 1e-9)).unwrap_or(10.0);
    m * 10f64.powf(e)
}

/// The round step after `step`.
fn nice_after(step: f64) -> f64 {
    nice_at_least(step * 1.5)
}

/// A range widened so that it has a width (a series of one value) and a margin of 5 % each side.
pub fn padded(lo: f64, hi: f64) -> (f64, f64) {
    let (lo, hi) = if lo <= hi { (lo, hi) } else { (hi, lo) };
    let span = hi - lo;
    if span.is_nan() || span <= 1e-12 * lo.abs().max(hi.abs()).max(1e-300) {
        let half = if lo == 0.0 { 0.5 } else { lo.abs() * 0.1 };
        return (lo - half, lo + half);
    }
    (lo - span * 0.05, hi + span * 0.05)
}

/// `n` intervals of a round step covering `lo..hi`: the smallest step for which they do.
fn cover(lo: f64, hi: f64, n: usize) -> Marks {
    let span = hi - lo;
    let mut step = nice_at_least(span / n as f64);
    loop {
        let start = (lo / step).floor() * step;
        if start + n as f64 * step >= hi - span * 1e-9 {
            return Marks { start, step, n };
        }
        step = nice_after(step);
    }
}

/// The axes for the data of the left (`left`) and the right (`right`) side, each range as it is: both padded, then
/// the same number of intervals (3 to `want` + 2, the one that wastes the least of both) on round steps each.
pub fn dual_axes(left: (f64, f64), right: (f64, f64), want: usize) -> DualAxes {
    let (l, r) = (padded(left.0, left.1), padded(right.0, right.1));
    let (lspan, rspan) = (l.1 - l.0, r.1 - r.0);
    let want = want.clamp(3, 8);
    let mut best: Option<(f64, DualAxes)> = None;
    for n in 3..=(want + 2).min(10) {
        let (ml, mr) = (cover(l.0, l.1, n), cover(r.0, r.1, n));
        let waste = (1.0 - lspan / (ml.step * n as f64)) + (1.0 - rspan / (mr.step * n as f64));
        let score = waste + 0.04 * (n as f64 - want as f64).abs();
        if best.as_ref().is_none_or(|b| score < b.0) {
            let a = ml.step / mr.step;
            best = Some((score, DualAxes { left: ml, right: mr, map: Affine { a, b: ml.start - mr.start * a } }));
        }
    }
    best.map(|b| b.1).expect("at least one interval count")
}

/// The unit of a lag: how far a series is shifted.
#[derive(Clone, Debug, PartialEq)]
pub enum Lag {
    /// a number of days (a duration in days or weeks)
    Days(f64),
    /// calendar months (a duration in months or years)
    Months(i32),
    /// a number of observations of the series
    Obs(usize),
}

impl Lag {
    /// A lag as the calculator writes it: `lag=2m`, `lag=30d`, `lag=2`.
    pub fn parse(text: &str) -> Option<Lag> {
        let t = text.trim();
        if let Ok(k) = t.parse::<usize>() {
            return (k > 0).then_some(Lag::Obs(k));
        }
        let unit = t.chars().last()?;
        let n: f64 = t[..t.len() - unit.len_utf8()].trim().parse().ok()?;
        if n.is_nan() || n <= 0.0 {
            return None;
        }
        match unit {
            'd' => Some(Lag::Days(n)),
            'w' => Some(Lag::Days(n * 7.0)),
            'm' if n.fract() == 0.0 => Some(Lag::Months(n as i32)),
            'y' if n.fract() == 0.0 => Some(Lag::Months(n as i32 * 12)),
            'm' => Some(Lag::Days(n * 30.4375)),
            'y' => Some(Lag::Days(n * 365.25)),
            _ => None,
        }
    }

    /// As written in a label: 2m, 30d, 2 obs.
    pub fn label(&self) -> String {
        match self {
            Lag::Days(d) => format!("{}d", janus_cube::multidim::format_g(*d, 6)),
            Lag::Months(m) if m % 12 == 0 => format!("{}y", m / 12),
            Lag::Months(m) => format!("{m}m"),
            Lag::Obs(k) => format!("{k} obs"),
        }
    }
}

/// The dates (Unix seconds) shifted forward by `lag`: days added, calendar months added (the day kept, or the month's
/// last), or the date of the observation `k` later (beyond the last, as many median steps).
pub fn shift_dates(xs: &[f64], lag: &Lag) -> Vec<f64> {
    match lag {
        Lag::Days(d) => xs.iter().map(|x| x + d * DAY).collect(),
        Lag::Months(m) => xs.iter().map(|&x| add_months(x, *m)).collect(),
        Lag::Obs(k) => {
            let step = median_gap(xs).unwrap_or(0.0);
            let n = xs.len();
            (0..n).map(|t| if t + k < n { xs[t + k] } else { xs[n - 1] + (t + k - (n - 1)) as f64 * step }).collect()
        }
    }
}

/// A date `m` calendar months later.
fn add_months(x: f64, m: i32) -> f64 {
    let days = (x / DAY).floor();
    let (y, mo, d) = civil_from_days(days as i64);
    let idx = i64::from(y) * 12 + i64::from(mo) - 1 + i64::from(m);
    let (ny, nm) = (idx.div_euclid(12) as i32, idx.rem_euclid(12) as u32 + 1);
    let (ay, am) = if nm == 12 { (ny + 1, 1) } else { (ny, nm + 1) };
    let in_month = (days_from_civil(ay, am, 1) - days_from_civil(ny, nm, 1)) as u32;
    days_from_civil(ny, nm, d.min(in_month)) as f64 * DAY + (x - days * DAY)
}

/// The median gap between consecutive dates (a bar's width is 0.8 of it), if there are two.
pub fn median_gap(xs: &[f64]) -> Option<f64> {
    let mut g: Vec<f64> = xs.windows(2).map(|w| w[1] - w[0]).filter(|g| *g > 0.0).collect();
    if g.is_empty() {
        return None;
    }
    g.sort_by(f64::total_cmp);
    Some(g[g.len() / 2])
}

/// How a layer's series are drawn in the chart (Time series > Series axes).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Look {
    /// against the right Y axis
    pub right: bool,
    /// as bars (from 0) in the layer's colours, whatever the chart's style
    pub bars: bool,
    /// shifted forward by the lag of a corr() of the calculator
    pub shift: Option<Lag>,
}

impl Look {
    /// What the legend says after the series' name.
    pub fn suffix(&self, right: bool) -> String {
        let mut s = String::new();
        if right {
            s += " (right)";
        }
        if let Some(l) = &self.shift {
            s += &format!(" (shifted +{})", l.label());
        }
        s
    }
}

/// A `corr(x, y, lag=k)` of a calculator's program: the layer of y (by its name) and the lag.
#[derive(Clone, Debug, PartialEq)]
pub struct CorrLag {
    pub y_alias: String,
    pub y_layer: Option<String>,
    pub lag: Lag,
}

/// The `corr(x, y, lag=k)` calls of a program's text (those with a lag): y's first name and the lag.
pub fn parse_corr_lags(text: &str) -> Vec<(String, Lag)> {
    let mut out = Vec::new();
    let b = text.as_bytes();
    let mut from = 0;
    while let Some(i) = text[from..].find("corr(") {
        let at = from + i;
        from = at + 5;
        if at > 0 && (b[at - 1].is_ascii_alphanumeric() || b[at - 1] == b'_') {
            continue;
        }
        // the arguments up to the matching parenthesis, split at the commas of the first level
        let mut depth = 1;
        let (mut args, mut start) = (Vec::new(), from);
        let mut end = None;
        for (k, c) in text[from..].char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        args.push(&text[start..from + k]);
                        end = Some(from + k);
                        break;
                    }
                }
                ',' if depth == 1 => {
                    args.push(&text[start..from + k]);
                    start = from + k + 1;
                }
                _ => {}
            }
        }
        let Some(end) = end else { break };
        from = end;
        let positional: Vec<&str> = args.iter().map(|a| a.trim()).filter(|a| !is_keyword(a)).collect();
        let lag = args.iter().find_map(|a| a.trim().strip_prefix("lag").and_then(|r| r.trim_start().strip_prefix('=')));
        let (Some(y), Some(lag)) = (positional.get(1), lag.and_then(Lag::parse)) else { continue };
        let name: String = y.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        if !name.is_empty() {
            out.push((name, lag));
        }
    }
    out
}

/// An argument `name=value` (not `==`).
fn is_keyword(a: &str) -> bool {
    let name: String = a.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    !name.is_empty() && a[name.len()..].trim_start().starts_with('=') && !a[name.len()..].trim_start().starts_with("==")
}

/// A series' corr() lags and when to look again (none found yet).
type Known = (Vec<CorrLag>, Option<std::time::Instant>);

/// The corr() lags of a calculator result's program, read from the run's provenance (kept by series: the file is
/// read once; a series without one is looked at again after a few seconds, the run may still be writing it).
pub fn corr_lags_of(info: &janus_cube::CubeInfo) -> Vec<CorrLag> {
    use std::time::{Duration, Instant};
    thread_local! {
        static KNOWN: std::cell::RefCell<HashMap<u64, Known>> = Default::default();
    }
    KNOWN.with_borrow_mut(|k| {
        if let Some((v, retry)) = k.get(&info.id)
            && retry.is_none_or(|t| Instant::now() < t)
        {
            return v.clone();
        }
        let v = read_corr_lags(info);
        let retry = v.is_empty().then(|| Instant::now() + Duration::from_secs(3));
        k.insert(info.id, (v.clone(), retry));
        v
    })
}

fn read_corr_lags(info: &janus_cube::CubeInfo) -> Vec<CorrLag> {
    let Some(first) = info.layers.first() else { return Vec::new() };
    let file = janus_cube::dataset_file(&first.path);
    let Some(dir) = crate::provenance::find(file) else { return Vec::new() };
    let Ok(p) = crate::provenance::Provenance::read(&dir) else { return Vec::new() };
    if p.tool != "calculator" {
        return Vec::new();
    }
    let text = p.extra.get("text").and_then(|t| t.as_str()).unwrap_or_default();
    // "alias = layer name"
    let names: Vec<(String, String)> = p
        .extra
        .get("layers")
        .and_then(|l| l.as_array())
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str()?.split_once(" = ").map(|(a, n)| (a.to_string(), n.to_string())))
        .collect();
    parse_corr_lags(text)
        .into_iter()
        .map(|(alias, lag)| CorrLag {
            y_layer: names.iter().find(|(a, _)| *a == alias).map(|(_, n)| n.clone()),
            y_alias: alias,
            lag,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn affine_round_trip() {
        let m = Affine { a: 0.002, b: -0.5 };
        for y in [0.0, 1.0, 123.4, -7.0] {
            assert!((m.inv(m.fwd(y)) - y).abs() < 1e-12);
        }
    }

    #[test]
    fn two_scales_share_their_marks() {
        // 0-1 on the left and 0-300 on the right: the same number of intervals, round steps, the inverse map
        // giving the right axis' marks
        let d = dual_axes((0.0, 1.0), (0.0, 300.0), 5);
        assert_eq!(d.left.n, d.right.n);
        let (lv, rv) = (d.left.values(), d.right.values());
        for (l, r) in lv.iter().zip(&rv) {
            assert!((d.map.inv(*l) - r).abs() < 1e-9 * r.abs().max(1.0), "{l} {r}");
        }
        // the marks cover the data (the left from 0 to 1, the right from 0 to 300)
        assert!(d.left.start <= 0.0 && d.left.end() >= 1.0);
        assert!(d.right.start <= 0.0 && d.right.end() >= 300.0);
        // round steps: 1, 2 or 5 times a power of ten
        for s in [d.left.step, d.right.step] {
            let m = s / 10f64.powf(s.log10().floor());
            assert!([1.0, 2.0, 5.0].iter().any(|c| (m - c).abs() < 1e-9), "{s}");
        }
        // a mark's text is the inverse map's value
        let labels: Vec<String> = lv.iter().map(|l| d.right_label(*l)).collect();
        let want: Vec<String> = rv.iter().map(|r| format!("{r:.*}", d.right.decimals())).collect();
        assert_eq!(labels, want);
        assert!(labels.iter().any(|t| t == "0") && labels.last().unwrap().parse::<f64>().unwrap() >= 300.0);
    }

    #[test]
    fn degenerate_range_has_a_width() {
        // a series of one value (min == max), and one of 0
        let (lo, hi) = padded(4.0, 4.0);
        assert!(lo < 4.0 && hi > 4.0);
        let (lo, hi) = padded(0.0, 0.0);
        assert!(lo < 0.0 && hi > 0.0);
        let d = dual_axes((2.0, 2.0), (0.0, 0.0), 5);
        assert!(d.map.a.is_finite() && d.map.a > 0.0 && d.map.b.is_finite());
        assert!(d.left.start <= 2.0 && d.left.end() >= 2.0 && d.right.start <= 0.0 && d.right.end() >= 0.0);
        // negative and mixed ranges
        let d = dual_axes((-3.0, 5.0), (-0.002, 0.001), 4);
        assert!(d.right.start <= -0.002 && d.right.end() >= 0.001);
        assert_eq!(d.right_label(d.map.fwd(0.0)), format!("{:.*}", d.right.decimals(), 0.0));
    }

    #[test]
    fn lags_parse() {
        assert_eq!(Lag::parse("2m"), Some(Lag::Months(2)));
        assert_eq!(Lag::parse("1y"), Some(Lag::Months(12)));
        assert_eq!(Lag::parse("30d"), Some(Lag::Days(30.0)));
        assert_eq!(Lag::parse("2w"), Some(Lag::Days(14.0)));
        assert_eq!(Lag::parse("3"), Some(Lag::Obs(3)));
        assert_eq!(Lag::parse("0"), None);
        assert_eq!(Lag::parse("x"), None);
        assert_eq!(Lag::Months(2).label(), "2m");
        assert_eq!(Lag::Months(24).label(), "2y");
        assert_eq!(Lag::Obs(2).label(), "2 obs");
    }

    #[test]
    fn shifts_by_days_months_and_observations() {
        let d = |y, m, dd| days_from_civil(y, m, dd) as f64 * DAY;
        let xs = [d(2020, 1, 31), d(2020, 11, 15), d(2021, 1, 15)];
        // days
        let s = shift_dates(&xs, &Lag::Days(30.0));
        assert_eq!(s[0], d(2020, 3, 1));
        // months: the day kept, or the last of a shorter month; across years
        let s = shift_dates(&xs, &Lag::Months(1));
        assert_eq!(s, vec![d(2020, 2, 29), d(2020, 12, 15), d(2021, 2, 15)]);
        let s = shift_dates(&xs, &Lag::Months(3));
        assert_eq!(s[1], d(2021, 2, 15));
        assert_eq!(shift_dates(&xs, &Lag::Months(12))[0], d(2021, 1, 31));
        // the time of the day is kept
        let t = d(2020, 5, 10) + 3600.0;
        assert_eq!(shift_dates(&[t], &Lag::Months(2))[0], d(2020, 7, 10) + 3600.0);
        // observations: the date of the one k later, beyond the last by median steps
        let q = [0.0, 10.0 * DAY, 20.0 * DAY, 30.0 * DAY];
        let s = shift_dates(&q, &Lag::Obs(2));
        assert_eq!(s, vec![20.0 * DAY, 30.0 * DAY, 40.0 * DAY, 50.0 * DAY]);
        assert_eq!(median_gap(&q), Some(10.0 * DAY));
        assert_eq!(median_gap(&q[..1]), None);
    }

    #[test]
    fn corr_calls_in_a_program() {
        let t = "out = corr(ndvi, chirps.prcp, lag=2m)\nz = corr(a, b, method=spearman)\nw = my_corr(a, b, lag=3)";
        assert_eq!(parse_corr_lags(t), vec![("chirps".to_string(), Lag::Months(2))]);
        let t = "corr(x, f(y, 1), lag=30d, method=pearson)";
        assert_eq!(parse_corr_lags(t), vec![("f".to_string(), Lag::Days(30.0))]);
        assert_eq!(parse_corr_lags("corr(x, y, lag=2)"), vec![("y".to_string(), Lag::Obs(2))]);
        assert!(parse_corr_lags("corr(x, y, lag=0)").is_empty());
    }

    #[test]
    fn look_suffix() {
        let l = Look { right: true, bars: false, shift: Some(Lag::Months(2)) };
        assert_eq!(l.suffix(true), " (right) (shifted +2m)");
        assert_eq!(l.suffix(false), " (shifted +2m)");
        assert_eq!(Look::default().suffix(false), "");
    }
}
