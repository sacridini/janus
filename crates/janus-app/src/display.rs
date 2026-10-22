//! What the map shows: the date, the display mode, its colormap and stretch, and the histogram of the shown
//! values (src/app.cpp: collectSample, autoRange, updateRangeAndHistogram).

use janus_gpu::{LayerParams, Mode, colormaps};

use crate::season::Season;
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

/// A spectrum's RGB mode by its name (app.cpp, modeName).
pub fn mode_name_of(m: Mode, info: &janus_cube::CubeInfo) -> &'static str {
    if m == Mode::Rgb && info.spectral { "RGB composite (3 bands)" } else { mode_name(m) }
}

/// The order of the modes in the menus (the enum's numbers are fixed by the shaders).
pub const MODE_ORDER: [Mode; 14] = [
    Mode::Value,
    Mode::Anomaly,
    Mode::Diff,
    Mode::Composite,
    Mode::Mean,
    Mode::Std,
    Mode::Slope,
    Mode::Min,
    Mode::Max,
    Mode::Amplitude,
    Mode::R2,
    Mode::DropDate,
    Mode::DropMag,
    Mode::Rgb,
];

/// A value of `mode` on a colour bar: dates for the drop's date.
pub fn range_label(mode: Mode, v: f32, info: &janus_cube::CubeInfo) -> String {
    if mode == Mode::DropDate {
        return info.layers[(v.round().max(0.0) as usize).min(info.t() - 1)].label.clone();
    }
    janus_cube::multidim::format_g(f64::from(v), 4)
}

pub const REDUCERS: [&str; 5] = ["Median", "Mean", "Maximum", "Minimum", "Observations"];

/// Modes whose values change with the date.
pub fn time_dependent(m: Mode) -> bool {
    matches!(m, Mode::Value | Mode::Anomaly | Mode::Rgb | Mode::Diff | Mode::Composite)
}

#[derive(Clone, Copy)]
pub struct Range {
    pub lo: f32,
    pub hi: f32,
    /// set by hand: not recomputed
    pub manual: bool,
    key: u64,
}

impl Range {
    /// Computed again from the next sample (unless set by hand).
    pub fn invalidate(&mut self) {
        self.key = u64::MAX;
    }
}

/// 0-1 until a sample sets it (the C++'s): a mode without values (no drop in the series) keeps it.
impl Default for Range {
    fn default() -> Self {
        Range { lo: 0.0, hi: 1.0, manual: false, key: u64::MAX }
    }
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
    /// the reference date of the difference: a date, or -1 the previous one
    pub diff_ref: i32,
    /// the automatic stretch of each date (else the same for every date: colours comparable through time)
    pub per_date_range: bool,
    /// past the overview's resolution, tiles of the visible area read at full resolution
    pub detail: bool,
    /// playback (dates per second)
    pub fps: f32,
    pub rgb: [usize; 3],
    /// the temporal composite's reducer (REDUCERS) and window: 0 the date's (season) year, 1 `days` around the
    /// date, 2 the whole series
    pub reducer: i32,
    pub window: i32,
    pub days: i32,
    /// part of the year: the other dates left out of the statistics, the composites, playback and the arrows
    pub season: Season,
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
            per_date_range: false,
            detail: true,
            fps: 4.0,
            rgb: [0, 0, 0],
            reducer: 0,
            window: 0,
            days: 30,
            season: Season::default(),
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
    /// The difference's reference date at date t: the fixed one, or the previous (none at the first).
    pub fn diff_ref_date(&self, t: usize, n: usize) -> Option<usize> {
        if self.diff_ref < 0 { t.checked_sub(1) } else { Some((self.diff_ref as usize).min(n - 1)) }
    }

    /// How the active layer is drawn (the class table and the warp are the caller's to add).
    pub fn layer_params(&self, s: &Session) -> LayerParams {
        let t = self.t as i32;
        let layers = match self.mode {
            Mode::Diff => [t, self.diff_ref_date(self.t, s.info.t()).map_or(t, |r| r as i32), 0],
            Mode::Rgb => self.rgb.map(|v| v as i32),
            Mode::Composite => {
                let (a, b) = self.temporal_window(&s.info, self.t);
                [a as i32, b as i32, self.reducer]
            }
            _ => [t, 0, 0],
        };
        let r = self.ranges[self.mode as usize];
        LayerParams::new(self.mode, layers, [r.lo, r.hi])
    }

    /// Whether what the mode draws is on the GPU: its dates, its statistics, the composite's window (C++
    /// renderMap).
    pub fn ready(&self, s: &Session, p: &LayerParams) -> bool {
        let up = |t: i32| s.uploaded.get(t as usize).copied().unwrap_or(false);
        match self.mode {
            Mode::Rgb => p.layers.iter().all(|&l| up(l)),
            Mode::Diff => up(self.t as i32) && self.diff_ref_date(self.t, s.info.t()).is_some_and(|r| up(r as i32)),
            Mode::Composite => (p.layers[0]..=p.layers[1]).all(up),
            m if m.needs_stats() => s.stats.is_some(),
            _ => up(self.t as i32),
        }
    }

    /// The composite of an overview pixel over dates t0..t1 (those in the season), as the shader computes it.
    pub fn composite_at(&self, s: &Session, p: usize, t0: usize, t1: usize) -> f32 {
        let ov = &s.s.overview;
        let vals: Vec<f32> = (t0..=t1)
            .filter(|&t| self.season.contains(&s.info, t) && s.uploaded[t])
            .filter_map(|t| ov.layer(t).map(|l| l[p]))
            .filter(|v| !v.is_nan())
            .collect();
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
        self.collect_sample_of(self.mode, s, t, max_n)
    }

    /// The automatic stretch of `mode` over every date (a map panel's own mode); None without values yet.
    pub fn auto_range_of(&self, mode: Mode, s: &Session) -> Option<(f32, f32)> {
        let mut sample = self.collect_sample_of(mode, s, None, 500_000);
        (!sample.is_empty()).then(|| auto_range(mode, &mut sample))
    }

    /// A sample of the values `mode` shows: of date `t`, or of every date loaded.
    fn collect_sample_of(&self, mode: Mode, s: &Session, t: Option<usize>, max_n: usize) -> Vec<f32> {
        let ov = &s.s.overview;
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
        match (mode, stats) {
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
                // every date against its reference; a date against itself (all zeros) only counts when it is the
                // date asked for
                let n = s.info.t();
                over_layers(&mut out, &|l, p| match self.diff_ref_date(l, n) {
                    Some(r) if s.uploaded[r] && (r != l || t.is_some()) => layer(l, p) - layer(r, p),
                    _ => f32::NAN,
                });
            }
            (Mode::Composite, _) => {
                let (t0, t1) = self.temporal_window(&s.info, t.unwrap_or(self.t));
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
        // the composite's numbers change with its options and the season (not with the date: the stretch is kept
        // so the years compare); the statistics' with each computation (a season)
        let comp_key = if self.mode == Mode::Composite {
            (((self.reducer as u64) * 3 + self.window as u64) * 1000 + self.days as u64)
                .wrapping_mul(1000003)
                .wrapping_add(self.season.key())
        } else {
            0
        };
        let version = (s.s.overview.layers_done() as u64 * 2 + u64::from(s.stats.is_some()))
            .wrapping_add(s.stats_gen.wrapping_mul(7919))
            .wrapping_add(comp_key.wrapping_mul(104729));
        let time_dep = time_dependent(self.mode);
        // the reference changes the values; a range per date follows the date
        let ref_key = if self.mode == Mode::Diff { (self.diff_ref + 2) as u64 * 7919 } else { 0 };
        let range_t = (self.per_date_range && time_dep).then_some(self.t);
        let rkey = version
            .wrapping_mul(1000003)
            .wrapping_add(range_t.map_or(0, |t| t as u64 + 1) * 8)
            .wrapping_add(u64::from(self.per_date_range))
            .wrapping_add(ref_key.wrapping_mul(65536))
            .wrapping_add((self.mode as u64) << 56);
        let mut changed = false;
        let r = &mut self.ranges[self.mode as usize];
        if !r.manual && r.key != rkey {
            let mut sample = self.collect_sample(s, range_t, 2_000_000);
            if !sample.is_empty() {
                let (lo, hi) = auto_range(self.mode, &mut sample);
                let r = &mut self.ranges[self.mode as usize];
                (r.lo, r.hi, r.key) = (lo, hi, rkey);
                changed = true;
            }
        }
        let hist_t = time_dep.then_some(if self.mode == Mode::Rgb { self.rgb[0] } else { self.t });
        let (c0, c1) = if self.mode == Mode::Composite { self.temporal_window(&s.info, self.t) } else { (0, 0) };
        let hkey = rkey
            .wrapping_mul(31)
            .wrapping_add(hist_t.map_or(0, |t| t as u64 + 1))
            .wrapping_add((c0 as u64 * 65537 + c1 as u64).wrapping_mul(31) << 20);
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
                    // as the C++ bins them: truncated toward zero
                    let b = ((f64::from(v) - f64::from(lo)) / w).trunc();
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
