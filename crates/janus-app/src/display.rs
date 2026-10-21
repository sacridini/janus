//! What the map shows: the date, the display mode, its colormap and stretch, and the histogram of the shown
//! values (src/app.cpp: collectSample, autoRange, updateRangeAndHistogram).

use janus_gpu::{DrawParams, Mode, colormaps};

use crate::session::Session;

/// The C++'s names of the modes (app.cpp, kModeNames).
pub fn mode_name(m: Mode) -> &'static str {
    match m {
        Mode::Value => "Value at date",
        Mode::Anomaly => "Anomaly (value - mean)",
        Mode::Mean => "Temporal mean",
        Mode::Std => "Temporal std. deviation",
        Mode::Slope => "Linear trend (OLS)",
        Mode::Min => "Temporal minimum",
        Mode::Max => "Temporal maximum",
        Mode::Amplitude => "Amplitude (max - min)",
        Mode::R2 => "Trend R²",
        Mode::Rgb => "Multitemporal RGB (3 dates)",
        Mode::Diff => "Difference (value - reference)",
        Mode::DropDate => "Largest drop: date",
        Mode::DropMag => "Largest drop: magnitude",
        Mode::Composite => "Temporal composite",
    }
}

pub const REDUCERS: [&str; 5] = ["Median", "Mean", "Maximum", "Minimum", "Observations"];

/// Modes whose values change with the date.
pub fn time_dependent(m: Mode) -> bool {
    matches!(m, Mode::Value | Mode::Anomaly | Mode::Rgb | Mode::Diff | Mode::Composite)
}

#[derive(Clone, Copy, Default)]
pub struct Range {
    pub lo: f32,
    pub hi: f32,
    /// set by hand: not recomputed
    pub manual: bool,
    key: u64,
}

#[derive(Default)]
pub struct Histogram {
    key: u64,
    /// bar centres, their share (%) and width
    pub xs: Vec<f64>,
    pub ys: Vec<f64>,
    pub bar_w: f64,
}

pub struct Display {
    pub t: usize,
    pub mode: Mode,
    pub cmap: [usize; 14],
    pub ranges: [Range; 14],
    /// the reference date of the difference
    pub diff_ref: usize,
    pub rgb: [usize; 3],
    /// the composite's reducer (REDUCERS) and window: the date's year or the whole series
    pub reducer: i32,
    pub whole_series: bool,
    pub hist: Histogram,
    pub playing: bool,
}

impl Default for Display {
    fn default() -> Self {
        let mut cmap = [colormaps::VIRIDIS; 14];
        for m in [Mode::Anomaly, Mode::Slope, Mode::Diff] {
            cmap[m as usize] = colormaps::BRBG;
        }
        for m in [Mode::Std, Mode::Amplitude, Mode::DropMag] {
            cmap[m as usize] = colormaps::PLASMA;
        }
        Display {
            t: 0,
            mode: Mode::Value,
            cmap,
            ranges: [Range::default(); 14],
            diff_ref: 0,
            rgb: [0, 0, 0],
            reducer: 0,
            whole_series: false,
            hist: Histogram::default(),
            playing: false,
        }
    }
}

/// The q-quantiles of a sample, as the C++ takes them (nth_element at floor(q (n - 1))).
fn percentiles(s: &mut [f32], q_lo: f64, q_hi: f64) -> (f32, f32) {
    if s.is_empty() {
        return (0.0, 1.0);
    }
    let mut q = |p: f64| {
        let k = (p * (s.len() - 1) as f64) as usize;
        *s.select_nth_unstable_by(k, f32::total_cmp).1
    };
    let (lo, mut hi) = (q(q_lo), q(q_hi));
    if hi.partial_cmp(&lo) != Some(std::cmp::Ordering::Greater) {
        hi = lo + 1e-6;
    }
    (lo, hi)
}

/// Automatic stretch of a sample: 2-98 %, symmetric around 0 for anomalies, differences and trends, 0-1 for R²,
/// every date for the drop's date.
fn auto_range(mode: Mode, s: &mut [f32]) -> (f32, f32) {
    match mode {
        Mode::R2 => (0.0, 1.0),
        Mode::DropDate => percentiles(s, 0.0, 1.0),
        Mode::DropMag => {
            let (_, hi) = percentiles(s, 0.0, 0.98);
            (0.0, if hi > 0.0 { hi } else { 1e-6 })
        }
        Mode::Anomaly | Mode::Slope | Mode::Diff => {
            s.iter_mut().for_each(|v| *v = v.abs());
            let (_, hi) = percentiles(s, 0.0, 0.98);
            (-hi, hi)
        }
        _ => percentiles(s, 0.02, 0.98),
    }
}

impl Display {
    /// The composite's dates: those of the shown date's year, or the whole series.
    pub fn composite_window(&self, s: &Session, t: usize) -> (usize, usize) {
        let n = s.info.t();
        if self.whole_series || !s.info.time_is_date {
            return (0, n - 1);
        }
        let year = |i: usize| s.info.decimal_year(i).floor();
        let y = year(t);
        let first = (0..n).find(|&i| year(i) == y).unwrap_or(t);
        let last = (0..n).rev().find(|&i| year(i) == y).unwrap_or(t);
        (first, last)
    }

    pub fn draw_params(&self, s: &Session, rect: [f32; 4]) -> DrawParams {
        let t = self.t as i32;
        let layers = match self.mode {
            Mode::Diff => [t, self.diff_ref as i32, 0],
            Mode::Rgb => self.rgb.map(|v| v as i32),
            Mode::Composite => {
                let (a, b) = self.composite_window(s, self.t);
                [a as i32, b as i32, self.reducer]
            }
            _ => [t, 0, 0],
        };
        let r = self.ranges[self.mode as usize];
        DrawParams { rect, mode: self.mode, layers, range: [r.lo, r.hi], alpha: 1.0 }
    }

    /// The composite of an overview pixel over dates t0..t1, as the shader computes it.
    fn composite_at(&self, s: &Session, p: usize, t0: usize, t1: usize) -> f32 {
        let ov = &s.overview;
        let vals: Vec<f32> = (t0..=t1).filter_map(|t| ov.layer(t).map(|l| l[p])).filter(|v| !v.is_nan()).collect();
        let n = vals.len();
        match self.reducer {
            4 => n as f32,
            _ if n == 0 => f32::NAN,
            1 => vals.iter().sum::<f32>() / n as f32,
            2 => vals.iter().copied().fold(f32::MIN, f32::max),
            3 => vals.iter().copied().fold(f32::MAX, f32::min),
            _ => {
                let k = n.div_ceil(64);
                let mut v: Vec<f32> = vals.iter().step_by(k).take(64).copied().collect();
                v.sort_by(f32::total_cmp);
                let m = v.len();
                if m % 2 == 1 { v[m / 2] } else { 0.5 * (v[m / 2 - 1] + v[m / 2]) }
            }
        }
    }

    /// A sample of the values the mode shows: of date `t`, or of every date loaded.
    fn collect_sample(&self, s: &Session, t: Option<usize>, max_n: usize) -> Vec<f32> {
        let ov = &s.overview;
        let px = ov.w * ov.h;
        let mut out = Vec::new();
        let stats = s.stats.as_ref();
        let over_layers = |out: &mut Vec<f32>, get: &dyn Fn(usize, usize) -> f32| {
            let layers: Vec<usize> = match t {
                Some(t) => [t].into_iter().filter(|&t| s.uploaded[t]).collect(),
                None => (0..ov.t).filter(|&i| s.uploaded[i]).collect(),
            };
            if layers.is_empty() {
                return;
            }
            let total = layers.len() * px;
            let step = (total / max_n).max(1);
            let mut i = (step / 2) % px;
            while i < total {
                let v = get(layers[i / px], i % px);
                if !v.is_nan() {
                    out.push(v);
                }
                i += step;
            }
        };
        let over_pixels = |out: &mut Vec<f32>, get: &dyn Fn(usize) -> f32| {
            let step = (px / max_n).max(1);
            out.extend((0..px).step_by(step).map(get).filter(|v| !v.is_nan()));
        };
        let layer = |l: usize, p: usize| ov.layer(l).map_or(f32::NAN, |d| d[p]);
        match (self.mode, stats) {
            (Mode::Value | Mode::Rgb, _) => over_layers(&mut out, &layer),
            (Mode::Anomaly, Some(st)) => over_layers(&mut out, &|l, p| layer(l, p) - st.s0[p][0]),
            (Mode::Mean, Some(st)) => over_pixels(&mut out, &|p| st.s0[p][0]),
            (Mode::Std, Some(st)) => over_pixels(&mut out, &|p| st.s0[p][1]),
            (Mode::Slope, Some(st)) => over_pixels(&mut out, &|p| st.s0[p][2]),
            (Mode::Min, Some(st)) => over_pixels(&mut out, &|p| st.s1[p][0]),
            (Mode::Max, Some(st)) => over_pixels(&mut out, &|p| st.s1[p][1]),
            (Mode::Amplitude, Some(st)) => over_pixels(&mut out, &|p| st.s1[p][1] - st.s1[p][0]),
            (Mode::R2, Some(st)) => over_pixels(&mut out, &|p| st.s1[p][2]),
            (Mode::DropDate, Some(st)) => over_pixels(&mut out, &|p| st.s2[p]),
            (Mode::DropMag, Some(st)) => over_pixels(&mut out, &|p| st.s1[p][3]),
            (Mode::Diff, _) => {
                let r = self.diff_ref;
                if s.uploaded[r] {
                    // a date against itself (all zeros) only counts when it is the date asked for
                    over_layers(&mut out, &|l, p| {
                        if l != r || t.is_some() { layer(l, p) - layer(r, p) } else { f32::NAN }
                    });
                }
            }
            (Mode::Composite, _) => {
                let (t0, t1) = self.composite_window(s, t.unwrap_or(self.t));
                let step = (px / max_n.min(100_000)).max(1);
                let mut p = (step / 2) % px;
                while p < px {
                    let v = self.composite_at(s, p, t0, t1);
                    if !v.is_nan() {
                        out.push(v);
                    }
                    p += step;
                }
            }
            _ => {}
        }
        out
    }

    /// Recomputes the automatic stretch and the histogram when what they depend on changed. Returns whether the
    /// stretch changed (the map is drawn again).
    pub fn update(&mut self, s: &Session) -> bool {
        if self.mode.needs_stats() && s.stats.is_none() {
            return false;
        }
        let (c0, c1) = if self.mode == Mode::Composite { self.composite_window(s, self.t) } else { (0, 0) };
        let version = (s.overview.layers_done() as u64 * 2 + u64::from(s.stats.is_some()))
            ^ (u64::from(self.whole_series) << 40)
            ^ ((self.reducer as u64) << 44)
            ^ ((c0 as u64 * 65537 + c1 as u64) << 20);
        let ref_key = if self.mode == Mode::Diff { self.diff_ref as u64 + 2 } else { 0 };
        let rkey = version.wrapping_mul(1000003).wrapping_add(ref_key * 65536).wrapping_add(self.mode as u64);
        let mut changed = false;
        let r = &mut self.ranges[self.mode as usize];
        if !r.manual && r.key != rkey {
            let mut sample = self.collect_sample(s, None, 2_000_000);
            if !sample.is_empty() {
                let (lo, hi) = auto_range(self.mode, &mut sample);
                let r = &mut self.ranges[self.mode as usize];
                (r.lo, r.hi, r.key) = (lo, hi, rkey);
                changed = true;
            }
        }
        let hist_t = time_dependent(self.mode).then_some(if self.mode == Mode::Rgb { self.rgb[0] } else { self.t });
        let hkey = rkey.wrapping_mul(31).wrapping_add(hist_t.map_or(0, |t| t as u64 + 1));
        if hkey != self.hist.key {
            self.hist = Histogram { key: hkey, ..Default::default() };
            let mut sample = self.collect_sample(s, hist_t, 400_000);
            if sample.len() > 10 {
                let n = sample.len();
                let (mut lo, mut hi) = percentiles(&mut sample, 0.005, 0.995);
                let mut bins = 64;
                if self.mode == Mode::DropDate && s.info.t() <= 128 {
                    // one bar per date
                    bins = s.info.t();
                    (lo, hi) = (-0.5, bins as f32 - 0.5);
                }
                let w = f64::from(hi - lo) / bins as f64;
                let mut ys = vec![0.0; bins];
                for &v in &sample {
                    let b = ((f64::from(v) - f64::from(lo)) / w).floor();
                    if b >= 0.0 && (b as usize) < bins {
                        ys[b as usize] += 1.0;
                    }
                }
                self.hist.xs = (0..bins).map(|i| f64::from(lo) + (i as f64 + 0.5) * w).collect();
                self.hist.ys = ys.into_iter().map(|y| 100.0 * y / n as f64).collect();
                self.hist.bar_w = w;
            }
        }
        changed
    }
}
