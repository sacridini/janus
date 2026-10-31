//! Field delineation: the fields of a window of a Sentinel-2 series as parcels, by a network that tells each pixel
//! background, a field's interior or a field's boundary (`fieldnet`: Fields of The World's and WRI's Trazo models)
//! from two median composites of the season (window A about planting, window B about harvest), then the parcels
//! found in its probabilities.
//!
//! As the networks are trained and tested (ftw-tools' `ftw model test`, the FTW and Trazo datasets): red, green,
//! blue and NIR of window B then of window A, Sentinel-2 L2A digital numbers of processing baseline 03
//! (reflectance x 10000) divided by 3000, tiles whose borders (`pad`) are dropped but at the window's edges. The
//! Trazo checkpoints at the chips' resolution (their training's; upsampled twice, as `ftw inference run` does, their
//! test set's pixel IoU falls from 77 to 55 in Mato Grosso), FTW's upsampled twice (bilinear, corners aligned,
//! kornia's Resize; each pixel's class the upsampled pixel at its top left). The parcels: ftw-tools' (the
//! interior's connected pixels), or a watershed from the interiors that hands the boundary pixels to the fields
//! on either side (parcels that touch, as the fields do).

use std::collections::BinaryHeap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use burn::prelude::*;
use burn::tensor::activation::softmax;
use janus_cube::{CubeInfo, QaRule, with_thread_reader};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::data::InputBand;
use crate::fieldnet::{BACKGROUND, BOUNDARY, CLASSES, FieldNet, INTERIOR};

/// ftw-tools' divisor of the digital numbers.
pub const NORMALIZATION: f32 = 3000.0;

/// The four bands the network reads (red, green, blue, NIR) and how their values become baseline-03 digital
/// numbers: dn = value * scale - offset.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct FieldBands {
    pub bands: Vec<InputBand>,
    /// 10000 for reflectance 0-1, 1 for digital numbers
    pub scale: f32,
    /// 1000 for raw digital numbers of processing baseline 04 and later (their BOA_ADD_OFFSET), else 0
    pub offset: f32,
}

/// Band names (any case, no spaces, dashes or underscores) of red, green, blue and NIR (zeit's aliases).
const NAMES: [&[&str]; 4] = [
    &["red", "b04", "b4", "sr_b4"],
    &["green", "b03", "b3", "sr_b3"],
    &["blue", "b02", "b2", "sr_b2"],
    &["nir", "b08", "b8", "nir08", "b8a", "sr_b5"],
];
const ROLES: [&str; 4] = ["red", "green", "blue", "nir"];

impl FieldBands {
    /// The series' red, green, blue and NIR: by name, else by Janus's band roles; the units from `high` (a high
    /// quantile of the red and NIR values: reflectance when below 2).
    pub fn of(info: &CubeInfo) -> Result<FieldBands, String> {
        let norm = |s: &str| s.to_lowercase().chars().filter(|c| !matches!(c, ' ' | '_' | '-')).collect::<String>();
        let names: Vec<String> = info.band_names.iter().map(|n| norm(n)).collect();
        let roles = janus_cube::roles::guess_band_roles(info);
        let mut bands = vec![];
        let mut missing = vec![];
        for (k, aliases) in NAMES.iter().enumerate() {
            let by_name = aliases.iter().find_map(|a| names.iter().position(|n| n == a)).map(|i| i as i32 + 1);
            match by_name.or_else(|| roles.get(ROLES[k]).copied()) {
                Some(b) => bands.push(InputBand::source(info, b)),
                None => missing.push(ROLES[k]),
            }
        }
        if !missing.is_empty() {
            return Err(format!(
                "the series has no {} band (its bands: {}): field delineation reads Sentinel-2's red, green, blue and NIR",
                missing.join(", "),
                if info.band_names.is_empty() { "unnamed".into() } else { info.band_names.join(", ") }
            ));
        }
        Ok(FieldBands { bands, scale: 1.0, offset: 0.0 })
    }

    /// The units from the red and NIR values seen (their 98th percentile): reflectance below 2.
    pub fn set_units(&mut self, high: f32) {
        self.scale = if high.is_finite() && high <= 2.0 { 1e4 } else { 1.0 };
    }

    pub fn to_input(&self, v: f32) -> f32 {
        if v.is_finite() { ((v * self.scale - self.offset).max(0.0)) / NORMALIZATION } else { 0.0 }
    }

    /// A value as a baseline-03 digital number (0 without: none).
    pub fn to_dn(&self, v: f32) -> u16 {
        if v.is_finite() { (v * self.scale - self.offset).round().clamp(1.0, 65535.0) as u16 } else { 0 }
    }
}

/// The dates of the two windows (indices of the series' dates).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Season {
    pub a: Vec<usize>,
    pub b: Vec<usize>,
}

/// The dates within [from, to] (days since 1970).
pub fn dates_between(info: &CubeInfo, from: i64, to: i64) -> Vec<usize> {
    (0..info.t()).filter(|&t| (from..=to).contains(&info.unix_day(t))).collect()
}

/// The season found in a series: the region's NDVI (the median over a grid of points) date by date, its peak, and
/// where it crosses 20 % of its rise before the peak (green-up: window A) and of its fall after (harvest: window B),
/// each window `half` days either side.
#[derive(Clone, Debug, Default)]
pub struct SeasonFound {
    pub season: Season,
    /// the day (since 1970) of window A's and B's middles and of the peak
    pub a_day: i64,
    pub b_day: i64,
    pub peak_day: i64,
    /// the region's NDVI on each date (NaN where too few points were clear)
    pub ndvi: Vec<f32>,
    /// the 98th percentile of the red and NIR values (the units)
    pub high: f32,
}

/// The region's NDVI and the season in it; `within` limits the peak to these days (a crop year), `half` is the
/// windows' half width in days.
pub fn find_season(
    info: &Arc<CubeInfo>,
    bands: &FieldBands,
    window: [usize; 4],
    within: Option<(i64, i64)>,
    half: i64,
) -> Result<SeasonFound, String> {
    if !info.time_is_date || info.t() < 3 {
        return Err("finding the season needs a series of dates".into());
    }
    let [x0, y0, x1, y1] = window;
    let (w, h) = (x1 - x0, y1 - y0);
    let n = 24usize;
    let points: Vec<(usize, usize)> =
        (0..n * n).map(|i| (x0 + ((i % n) * 2 + 1) * w / (2 * n), y0 + ((i / n) * 2 + 1) * h / (2 * n))).collect();
    let rn = [bands.bands[0].clone(), bands.bands[3].clone()];
    let s = crate::data::read_points(info, &rn, &points);
    let t = info.t();
    let mut seen: Vec<f32> = s.values.iter().copied().filter(|v| v.is_finite()).collect();
    seen.sort_by(f32::total_cmp);
    let high = if seen.is_empty() { f32::NAN } else { seen[(seen.len() - 1) * 98 / 100] };
    let ndvi: Vec<f32> = (0..t)
        .map(|d| {
            let mut v: Vec<f32> = (0..s.n)
                .filter_map(|i| {
                    let (r, nir) = (s.values[(i * t + d) * 2], s.values[(i * t + d) * 2 + 1]);
                    (r.is_finite() && nir.is_finite() && r + nir > 0.0).then(|| (nir - r) / (nir + r))
                })
                .collect();
            if v.len() * 5 < s.n {
                return f32::NAN;
            }
            v.sort_by(f32::total_cmp);
            v[v.len() / 2]
        })
        .collect();
    let days: Vec<i64> = (0..t).map(|d| info.unix_day(d)).collect();
    let inside = |d: usize| within.is_none_or(|(a, b)| (a..=b).contains(&days[d]));
    let peak = (0..t)
        .filter(|&d| ndvi[d].is_finite() && inside(d))
        .max_by(|&a, &b| ndvi[a].total_cmp(&ndvi[b]))
        .ok_or("no date with enough clear pixels to see the season")?;
    // the lowest NDVI within 200 days before and after the peak
    let low = |range: Vec<usize>| {
        range.into_iter().filter(|&d| ndvi[d].is_finite()).min_by(|&a, &b| ndvi[a].total_cmp(&ndvi[b]))
    };
    let before = low((0..peak).filter(|&d| days[peak] - days[d] <= 200).collect());
    let after = low((peak + 1..t).filter(|&d| days[d] - days[peak] <= 200).collect());
    let cross = |trough: Option<usize>, rising: bool| -> i64 {
        let Some(tr) = trough else { return days[peak] + if rising { -90 } else { 90 } };
        let level = ndvi[tr] + 0.2 * (ndvi[peak] - ndvi[tr]);
        let range: Vec<usize> = if rising { (tr..=peak).collect() } else { (peak..=tr).rev().collect() };
        // the first date past the level, going from the trough to the peak
        let k = range.iter().position(|&d| ndvi[d].is_finite() && ndvi[d] >= level).unwrap_or(0);
        if k == 0 {
            return days[range[0]];
        }
        let (p, q) = (range[k - 1], range[k]);
        let (vp, vq) = (ndvi[p], ndvi[q]);
        let f = if vq != vp { ((level - vp) / (vq - vp)).clamp(0.0, 1.0) } else { 0.5 };
        days[p] + ((days[q] - days[p]) as f32 * f).round() as i64
    };
    let (a_day, b_day) = (cross(before, true), cross(after, false));
    let pick = |mid: i64| {
        let mut d = dates_between(info, mid - half, mid + half);
        if d.is_empty() {
            d = dates_between(info, mid - 2 * half, mid + 2 * half);
        }
        d
    };
    let season = Season { a: pick(a_day), b: pick(b_day) };
    if season.a.is_empty() || season.b.is_empty() {
        return Err(format!(
            "no date near the season's start ({}) or end ({}): choose the windows' dates",
            day_text(a_day),
            day_text(b_day)
        ));
    }
    Ok(SeasonFound { season, a_day, b_day, peak_day: days[peak], ndvi, high })
}

/// A day since 1970 as YYYY-MM-DD.
pub fn day_text(day: i64) -> String {
    let (y, m, d) = janus_cube::dates::civil_from_days(day);
    format!("{y:04}-{m:02}-{d:02}")
}

/// The 98th percentile of the red and NIR values on a grid of points of the window at the given dates (to tell
/// reflectance from digital numbers).
pub fn units_high(info: &Arc<CubeInfo>, bands: &FieldBands, window: [usize; 4], dates: &[usize]) -> f32 {
    let [x0, y0, x1, y1] = window;
    let (w, h) = (x1 - x0, y1 - y0);
    let n = 12usize;
    let mut seen = vec![];
    for &t in dates.iter().take(6) {
        let pts: Vec<(usize, usize)> =
            (0..n * n).map(|i| (x0 + ((i % n) * 2 + 1) * w / (2 * n), y0 + ((i / n) * 2 + 1) * h / (2 * n))).collect();
        for (x, y) in pts {
            let c = composite(info, &[bands.bands[0].clone(), bands.bands[3].clone()], &[t], (x, y, 1, 1));
            seen.extend(c.into_iter().filter(|v| v.is_finite()));
        }
    }
    seen.sort_by(f32::total_cmp);
    if seen.is_empty() { f32::NAN } else { seen[(seen.len() - 1) * 98 / 100] }
}

/// The tiles' settings: their side in the series' pixels (a multiple of 32), the border dropped from each side,
/// the upsampling, tiles a batch.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Tiling {
    pub patch: usize,
    pub pad: usize,
    pub resize: usize,
    pub batch: usize,
}

impl Default for Tiling {
    /// ftw-tools' for 256 px tiles: the border 16, upsampled twice.
    fn default() -> Self {
        Tiling { patch: 256, pad: 16, resize: 2, batch: 2 }
    }
}

/// The median of each band over the dates of a window, at each pixel of a tile (x, y, w, h): [band][h][w], NaN
/// where no date has a value. A date's band masked by the quality band (as the series shows it) has none.
pub fn composite(
    info: &Arc<CubeInfo>,
    bands: &[InputBand],
    dates: &[usize],
    win: (usize, usize, usize, usize),
) -> Vec<f32> {
    let n = win.2 * win.3;
    let sel = info.sel;
    let per_date: Vec<Vec<f32>> = dates
        .par_iter()
        .map(|&t| {
            with_thread_reader(info, |r| {
                let mut out = vec![f32::NAN; bands.len() * n];
                let mut qa = vec![];
                if sel.qa_band > 0 && sel.qa_rule != QaRule::None {
                    qa = vec![f32::NAN; n];
                    if !r.read_band_window(t, sel.qa_band, win, &mut qa, (win.2, win.3)) {
                        qa.fill(f32::NAN);
                    }
                }
                for (k, b) in bands.iter().enumerate() {
                    let buf = &mut out[k * n..(k + 1) * n];
                    if !r.read_band_window(t, b.band, win, buf, (win.2, win.3)) {
                        buf.fill(f32::NAN);
                    } else if !qa.is_empty() {
                        for (v, &q) in buf.iter_mut().zip(&qa) {
                            if q.is_nan() || q < 0.0 || !sel.qa_rule.usable(q as u32) {
                                *v = f32::NAN;
                            }
                        }
                    }
                }
                out
            })
        })
        .collect();
    let mut out = vec![f32::NAN; bands.len() * n];
    out.par_chunks_mut(n).enumerate().for_each(|(k, plane)| {
        let mut v = Vec::with_capacity(per_date.len());
        for (i, o) in plane.iter_mut().enumerate() {
            v.clear();
            v.extend(per_date.iter().map(|d| d[k * n + i]).filter(|x| x.is_finite()));
            if v.is_empty() {
                continue;
            }
            v.sort_by(f32::total_cmp);
            let m = v.len();
            *o = if m % 2 == 1 { v[m / 2] } else { 0.5 * (v[m / 2 - 1] + v[m / 2]) };
        }
    });
    out
}

/// Linear resampling of one dimension of n values to m with the corners aligned (PyTorch's bilinear with
/// align_corners=True, kornia's Resize): output o reads the input at o (n - 1) / (m - 1).
fn resample_aligned(x: Tensor<4>, dim: usize, m: usize) -> Tensor<4> {
    let n = x.dims()[dim];
    let device = x.device();
    let (mut i0, mut i1, mut f) = (vec![], vec![], vec![]);
    for o in 0..m {
        let src = if m > 1 { o as f64 * (n - 1) as f64 / (m - 1) as f64 } else { 0.0 };
        let a = (src.floor() as usize).min(n - 1);
        i0.push(a as i64);
        i1.push((a + 1).min(n - 1) as i64);
        f.push((src - a as f64) as f32);
    }
    let idx = |v: Vec<i64>| Tensor::<1, Int>::from_data(TensorData::new(v, [m]), &device);
    let mut shape = [1usize; 4];
    shape[dim] = m;
    let w1 = Tensor::<1>::from_data(TensorData::new(f, [m]), &device).reshape(shape);
    let w0 = w1.clone().neg() + 1.0;
    x.clone().select(dim, idx(i0)) * w0 + x.select(dim, idx(i1)) * w1
}

/// The network's class probabilities of images [b, 8, p, p] (inputs already normalized), at their resolution:
/// upsampled `resize` times, the scores' softmax, every `resize`-th pixel kept -> [b, 3, p, p].
pub fn probabilities(net: &FieldNet, x: Tensor<4>, resize: usize) -> Tensor<4> {
    let [_, _, h, w] = x.dims();
    if resize <= 1 {
        return softmax(net.forward(x), 1);
    }
    let up = resample_aligned(resample_aligned(x, 2, h * resize), 3, w * resize);
    let p = softmax(net.forward(up), 1);
    let device = p.device();
    let keep = |n: usize| {
        Tensor::<1, Int>::from_data(
            TensorData::new((0..n).map(|i| (i * resize) as i64).collect::<Vec<_>>(), [n]),
            &device,
        )
    };
    p.select(2, keep(h)).select(3, keep(w))
}

/// The tiles' origins along a side of n pixels: a step of patch - 2 pad, the last one ending at the side.
pub(crate) fn origins(n: usize, patch: usize, pad: usize) -> Vec<usize> {
    if n <= patch {
        return vec![0];
    }
    let stride = (patch - 2 * pad).max(1);
    let mut v: Vec<usize> = (0..=n - patch).step_by(stride).collect();
    if *v.last().unwrap() != n - patch {
        v.push(n - patch);
    }
    v
}

/// The probabilities of a window of the series: [class][h][w] as percent (0-100; 255 where neither window has a
/// value), the window's pixels row by row.
pub struct Probabilities {
    pub w: usize,
    pub h: usize,
    pub p: Vec<u8>,
    /// a temporal model's distance to the field's edge, percent of the field's largest (empty without)
    pub dist: Vec<u8>,
}

pub const NO_VALUE: u8 = 255;

impl Probabilities {
    pub fn at(&self, class: usize, i: usize) -> u8 {
        self.p[class * self.w * self.h + i]
    }

    /// Each pixel's class (0 background, 1 interior, 2 boundary; 255 without value).
    pub fn classes(&self) -> Vec<u8> {
        let n = self.w * self.h;
        (0..n)
            .map(|i| {
                let v = [self.p[i], self.p[n + i], self.p[2 * n + i]];
                if v[0] == NO_VALUE {
                    return NO_VALUE;
                }
                // ties as argmax: the first
                let mut best = 0;
                for k in 1..CLASSES {
                    if v[k] > v[best] {
                        best = k;
                    }
                }
                best as u8
            })
            .collect()
    }
}

/// The months a temporal field model takes: `months` months from the first (year, month 1-12), each with its dates
/// in the series (none when the series has none in it).
pub fn month_dates(info: &CubeInfo, first: (i32, u32), months: usize) -> Vec<Vec<usize>> {
    (0..months)
        .map(|k| {
            let m0 = first.0 as i64 * 12 + first.1 as i64 - 1 + k as i64;
            let (y, m) = ((m0.div_euclid(12)) as i32, (m0.rem_euclid(12) + 1) as u32);
            let (y1, m1) = (((m0 + 1).div_euclid(12)) as i32, ((m0 + 1).rem_euclid(12) + 1) as u32);
            let from = janus_cube::dates::days_from_civil(y, m, 1);
            let to = janus_cube::dates::days_from_civil(y1, m1, 1) - 1;
            dates_between(info, from, to)
        })
        .collect()
}

/// The months' positions for U-TAE: each month's middle in days from the first's start.
pub fn month_positions(months: usize) -> Vec<f64> {
    (0..months).map(|k| k as f64 * 30.44 + 15.0).collect()
}

/// A temporal field model (U-TAE: three classes and the distance) over a window of the series: each month the
/// median of its dates, the values as the FTW networks take them (`FieldBands::to_input`) standardized by the
/// model's; tiles of side p (a multiple of 8) overlapping by 2 pad.
#[allow(clippy::too_many_arguments)]
pub fn delineate_temporal(
    net: &crate::utae::Utae,
    standard: &crate::dense::Standard,
    device: &Device,
    info: &Arc<CubeInfo>,
    bands: &FieldBands,
    months: &[Vec<usize>],
    window: [usize; 4],
    patch: usize,
    batch: usize,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<Probabilities, String> {
    let p = patch.max(32) / 8 * 8;
    let pad = (p / 8).max(8);
    let [x0, y0, x1, y1] = window;
    let (w, h) = (x1 - x0, y1 - y0);
    let (t, c) = (months.len(), 4);
    let pos: Vec<f32> = month_positions(t).iter().map(|&d| d as f32).collect();
    tiles(window, p, pad, batch, CLASSES + 1, progress, cancel, &mut |chunk| {
        let b = chunk.len();
        let mut x = vec![f32::NAN; b * t * c * p * p];
        let mut valid = vec![vec![false; p * p]; b];
        for (bi, &(tx, ty)) in chunk.iter().enumerate() {
            let (tw, th) = (p.min(w), p.min(h));
            let win = (x0 + tx, y0 + ty, tw, th);
            for (m, dates) in months.iter().enumerate() {
                if dates.is_empty() {
                    continue;
                }
                let v = composite(info, &bands.bands, dates, win);
                for k in 0..c {
                    for yy in 0..th {
                        for xx in 0..tw {
                            let a = v[(k * th + yy) * tw + xx];
                            if a.is_finite() {
                                valid[bi][yy * p + xx] = true;
                                x[(((bi * t + m) * c + k) * p + yy) * p + xx] = bands.to_input(a);
                            }
                        }
                    }
                }
            }
        }
        standard.apply(&mut x, c, p * p);
        let input = Tensor::<5>::from_data(TensorData::new(x, [b, t, c, p, p]), device);
        let positions = Tensor::<2>::from_data(TensorData::new(pos.clone(), [1, t]), device).expand([b, t]);
        let scores = net.forward(input, positions);
        let probs = softmax(scores.clone().slice([0..b, 0..CLASSES, 0..p, 0..p]), 1);
        let dist = burn::tensor::activation::sigmoid(scores.slice([0..b, CLASSES..CLASSES + 1, 0..p, 0..p]));
        let out: Vec<f32> = Tensor::cat(vec![probs, dist], 1).into_data().try_to_vec().map_err(|e| format!("{e:?}"))?;
        Ok((out, valid))
    })
}

/// The network over a window of the series (x0, y0, x1, y1), tile by tile as ftw-tools does.
#[allow(clippy::too_many_arguments)]
pub fn delineate(
    net: &FieldNet,
    device: &Device,
    info: &Arc<CubeInfo>,
    bands: &FieldBands,
    season: &Season,
    window: [usize; 4],
    tiling: &Tiling,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<Probabilities, String> {
    let p = tiling.patch.max(32) / 32 * 32;
    let pad = tiling.pad.min(p / 4);
    let [x0, y0, x1, y1] = window;
    let (w, h) = (x1 - x0, y1 - y0);
    tiles(window, p, pad, tiling.batch, CLASSES, progress, cancel, &mut |chunk| {
        let b = chunk.len();
        let mut x = vec![0f32; b * 8 * p * p];
        let mut valid = vec![vec![false; p * p]; b];
        for (bi, &(tx, ty)) in chunk.iter().enumerate() {
            let (tw, th) = (p.min(w), p.min(h));
            let win = (x0 + tx, y0 + ty, tw, th);
            // window B first: the datasets' order
            for (wi, dates) in [&season.b, &season.a].into_iter().enumerate() {
                let c = composite(info, &bands.bands, dates, win);
                for k in 0..4 {
                    for yy in 0..th {
                        for xx in 0..tw {
                            let v = c[(k * th + yy) * tw + xx];
                            if v.is_finite() {
                                valid[bi][yy * p + xx] = true;
                            }
                            x[((bi * 8 + wi * 4 + k) * p + yy) * p + xx] = bands.to_input(v);
                        }
                    }
                }
            }
        }
        let input = Tensor::<4>::from_data(TensorData::new(x, [b, 8, p, p]), device);
        let probs: Vec<f32> =
            probabilities(net, input, tiling.resize).into_data().try_to_vec().map_err(|e| format!("{e:?}"))?;
        Ok((probs, valid))
    })
}

/// A tile batch's outputs: [b][planes][p][p] (the classes' probabilities, then the distance), and each tile's
/// pixels that had a value.
type TileOut = (Vec<f32>, Vec<Vec<bool>>);

/// The tiles of side p over a window (their borders `pad` dropped but at the window's edges), `run` giving a
/// batch's outputs: the probabilities (and a fourth plane, the distance, when `planes` is 4).
#[allow(clippy::too_many_arguments)]
/// A batch of tiles (their origins) through the network.
type TileRun<'a> = dyn FnMut(&[(usize, usize)]) -> Result<TileOut, String> + 'a;

#[allow(clippy::too_many_arguments)]
fn tiles(
    window: [usize; 4],
    p: usize,
    pad: usize,
    batch: usize,
    planes: usize,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
    run: &mut TileRun,
) -> Result<Probabilities, String> {
    let [x0, y0, x1, y1] = window;
    let (w, h) = (x1 - x0, y1 - y0);
    let (xs, ys) = (origins(w, p, pad), origins(h, p, pad));
    let tiles: Vec<(usize, usize)> = ys.iter().flat_map(|&y| xs.iter().map(move |&x| (x, y))).collect();
    let n = w * h;
    let mut out = Probabilities {
        w,
        h,
        p: vec![NO_VALUE; CLASSES * n],
        dist: if planes > CLASSES { vec![NO_VALUE; n] } else { vec![] },
    };
    let t0 = std::time::Instant::now();
    for (done, chunk) in tiles.chunks(batch.max(1)).enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let (probs, valid) = run(chunk)?;
        for (bi, &(tx, ty)) in chunk.iter().enumerate() {
            let (tw, th) = (p.min(w), p.min(h));
            // the tile's kept part: its border dropped but at the window's edges
            let l = if tx == 0 { 0 } else { pad };
            let r = if tx + tw >= w { tw } else { tw - pad };
            let top = if ty == 0 { 0 } else { pad };
            let bot = if ty + th >= h { th } else { th - pad };
            for yy in top..bot {
                for xx in l..r {
                    let i = (ty + yy) * w + tx + xx;
                    if !valid[bi][yy * p + xx] {
                        continue;
                    }
                    for k in 0..planes {
                        let v = probs[((bi * planes + k) * p + yy) * p + xx];
                        let v = (v * 100.0).round().clamp(0.0, 100.0) as u8;
                        if k < CLASSES {
                            out.p[k * n + i] = v;
                        } else {
                            out.dist[i] = v;
                        }
                    }
                }
            }
        }
        let k = ((done + 1) * batch.max(1)).min(tiles.len());
        let el = t0.elapsed().as_secs_f64();
        progress(
            k as f64 / tiles.len() as f64,
            &format!(
                "tiles {k}/{}, {el:.0} s elapsed, ~{:.0} s left",
                tiles.len(),
                el / k as f64 * (tiles.len() - k) as f64
            ),
        );
    }
    Ok(out)
}

/// How the parcels are found in the probabilities.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum Parcels {
    /// a watershed from the interiors over the boundary probability: the boundary pixels go to the fields on
    /// either side, so neighbouring parcels touch
    #[default]
    Watershed,
    /// ftw-tools' polygonize: each set of connected interior pixels is a parcel (the boundaries between them left
    /// out)
    Interiors,
}

/// The parcels: a label per pixel of the window (0: none, from 1). `interior` is the probability (percent) from
/// which a pixel is a field's core in the watershed's seeds.
pub fn parcels(probs: &Probabilities, method: Parcels, interior: u8) -> Vec<u32> {
    let (w, h) = (probs.w, probs.h);
    let n = w * h;
    let classes = probs.classes();
    match method {
        Parcels::Interiors => components(&classes.iter().map(|&c| c as usize == INTERIOR).collect::<Vec<_>>(), w, h),
        Parcels::Watershed => {
            // seeds: the cores of the interiors (an interior pixel, probability at least `interior`, its four
            // neighbours interior too)
            // (with a temporal model's distance: the pixels at least `interior` percent of the way to the field's
            // middle, the flood the lowest distance last)
            let with_dist = probs.dist.len() == n;
            let strong =
                |j: usize| if with_dist { probs.dist[j] >= interior } else { probs.at(INTERIOR, j) >= interior };
            let core: Vec<bool> = (0..n)
                .map(|i| {
                    let (x, y) = (i % w, i / w);
                    let is = |j: usize| classes[j] as usize == INTERIOR && strong(j);
                    is(i)
                        && (x == 0 || is(i - 1))
                        && (x + 1 == w || is(i + 1))
                        && (y == 0 || is(i - w))
                        && (y + 1 == h || is(i + w))
                })
                .collect();
            let mut labels = components(&core, w, h);
            // an interior too narrow for a core seeds itself whole
            let interiors = components(&classes.iter().map(|&c| c as usize == INTERIOR).collect::<Vec<_>>(), w, h);
            let count = interiors.iter().copied().max().unwrap_or(0) as usize;
            let mut has_core = vec![false; count + 1];
            for i in 0..n {
                if labels[i] > 0 {
                    has_core[interiors[i] as usize] = true;
                }
            }
            let mut next = labels.iter().copied().max().unwrap_or(0);
            let mut given = vec![0u32; count + 1];
            for i in 0..n {
                let c = interiors[i] as usize;
                if c > 0 && !has_core[c] {
                    if given[c] == 0 {
                        next += 1;
                        given[c] = next;
                    }
                    labels[i] = given[c];
                }
            }
            // the field (interior or boundary) flooded from the seeds, lowest boundary probability first
            let field: Vec<bool> = classes.iter().map(|&c| c != NO_VALUE && c as usize != BACKGROUND).collect();
            let mut heap = BinaryHeap::new();
            let push = |heap: &mut BinaryHeap<(i32, u64, u32)>, i: usize, order: &mut u64| {
                *order += 1;
                let height = if with_dist { 100 - probs.dist[i].min(100) as i32 } else { probs.at(BOUNDARY, i) as i32 };
                heap.push((-height, u64::MAX - *order, i as u32));
            };
            let mut order = 0u64;
            for (i, &l) in labels.iter().enumerate().take(n) {
                if l > 0 {
                    push(&mut heap, i, &mut order);
                }
            }
            while let Some((_, _, i)) = heap.pop() {
                let i = i as usize;
                let (x, y) = (i % w, i / w);
                let l = labels[i];
                let mut nb = [usize::MAX; 4];
                if x > 0 {
                    nb[0] = i - 1;
                }
                if x + 1 < w {
                    nb[1] = i + 1;
                }
                if y > 0 {
                    nb[2] = i - w;
                }
                if y + 1 < h {
                    nb[3] = i + w;
                }
                for j in nb {
                    if j != usize::MAX && labels[j] == 0 && field[j] {
                        labels[j] = l;
                        push(&mut heap, j, &mut order);
                    }
                }
            }
            labels
        }
    }
}

/// The classes' names and colours in the class map (value k + 1 is class k).
pub const CLASS_NAMES: [&str; 3] = ["background", "field interior", "field boundary"];
pub const CLASS_COLORS: [[u8; 3]; 3] = [[230, 230, 220], [76, 168, 72], [140, 60, 20]];

/// The maps of a delineation written beside each other in `dir` (`stem`_class, _interior, _boundary, _parcels
/// .tif, the window's grid): their descriptions as a Zeit run's outputs. The parcels' map (UInt32, 0 none) is the
/// one polygonized.
pub fn write(
    dir: &std::path::Path,
    stem: &str,
    info: &CubeInfo,
    window: [usize; 4],
    probs: &Probabilities,
    labels: &[u32],
    model: &str,
) -> Result<Vec<serde_json::Value>, String> {
    use gdal::raster::{Buffer, RasterCreationOptions};
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let (w, h) = (probs.w, probs.h);
    let n = w * h;
    let path = |id: &str| dir.join(format!("{stem}_{id}.tif"));
    let write_byte = |id: &str, v: Vec<u8>, nodata: u8, classes: bool| -> Result<(), String> {
        let mut ds = crate::apply::create(&path(id), w, h, Some(nodata), info, window)?;
        if classes {
            let mut values = vec![("", [0u8, 0, 0])];
            values.extend(CLASS_NAMES.iter().zip(CLASS_COLORS).map(|(n, c)| (*n, c)));
            crate::apply::describe(&mut ds, &values, 0)?;
        }
        ds.rasterband(1)
            .map_err(|e| e.to_string())?
            .write((0, 0), (w, h), &mut Buffer::new((w, h), v))
            .map_err(|e| e.to_string())
    };
    let classes: Vec<u8> = probs.classes().into_iter().map(|c| if c == NO_VALUE { 0 } else { c + 1 }).collect();
    write_byte("class", classes, 0, true)?;
    write_byte("interior", probs.p[INTERIOR * n..(INTERIOR + 1) * n].to_vec(), NO_VALUE, false)?;
    write_byte("boundary", probs.p[BOUNDARY * n..(BOUNDARY + 1) * n].to_vec(), NO_VALUE, false)?;
    {
        let driver = gdal::DriverManager::get_driver_by_name("GTiff").map_err(|e| e.to_string())?;
        let opts = RasterCreationOptions::from_iter(["COMPRESS=DEFLATE", "TILED=YES", "BIGTIFF=IF_SAFER"]);
        let p = path("parcels");
        let mut ds = driver
            .create_with_band_type_with_options::<u32, _>(&p, w, h, 1, &opts)
            .map_err(|e| format!("{}: {e}", p.display()))?;
        if let Some(g) = info.geo_transform {
            let x = g[0] + window[0] as f64 * g[1] + window[1] as f64 * g[2];
            let y = g[3] + window[0] as f64 * g[4] + window[1] as f64 * g[5];
            ds.set_geo_transform(&[x, g[1], g[2], y, g[4], g[5]]).map_err(|e| e.to_string())?;
        }
        if !info.crs_wkt.is_empty() {
            ds.set_projection(&info.crs_wkt).map_err(|e| e.to_string())?;
        }
        let mut b = ds.rasterband(1).map_err(|e| e.to_string())?;
        b.set_no_data_value(Some(0.0)).map_err(|e| e.to_string())?;
        b.write((0, 0), (w, h), &mut Buffer::new((w, h), labels.to_vec())).map_err(|e| e.to_string())?;
    }
    let s = |id: &str| path(id).to_string_lossy().into_owned();
    let mut extra = vec![];
    if probs.dist.len() == n {
        write_byte("distance", probs.dist.clone(), NO_VALUE, false)?;
        extra.push(serde_json::json!({"id": "distance", "name": "Distance to the field's edge (% of its middle's)",
            "unit": "%", "colormap": "Viridis", "path": s("distance"), "hidden": true}));
    }
    Ok([
        vec![
            serde_json::json!({"id": "class", "name": "Fields (background, interior, boundary)", "unit": "class",
            "colormap": "Paired", "classes": CLASS_NAMES, "class_colors": CLASS_COLORS, "path": s("class"),
            "model": model}),
            serde_json::json!({"id": "interior", "name": "Field interior probability (%)", "unit": "%",
            "colormap": "Viridis", "path": s("interior"), "hidden": true}),
            serde_json::json!({"id": "boundary", "name": "Field boundary probability (%)", "unit": "%",
            "colormap": "Magma", "path": s("boundary"), "hidden": true}),
        ],
        extra,
    ]
    .concat())
}

/// The 4-connected components of a mask: a label per pixel (0 outside, from 1, in the order met row by row).
pub fn components(mask: &[bool], w: usize, h: usize) -> Vec<u32> {
    let mut labels = vec![0u32; w * h];
    let mut next = 0u32;
    let mut stack = vec![];
    for s in 0..w * h {
        if !mask[s] || labels[s] != 0 {
            continue;
        }
        next += 1;
        labels[s] = next;
        stack.push(s);
        while let Some(i) = stack.pop() {
            let (x, y) = (i % w, i / w);
            let mut visit = |j: usize| {
                if mask[j] && labels[j] == 0 {
                    labels[j] = next;
                    stack.push(j);
                }
            };
            if x > 0 {
                visit(i - 1);
            }
            if x + 1 < w {
                visit(i + 1);
            }
            if y > 0 {
                visit(i - w);
            }
            if y + 1 < h {
                visit(i + w);
            }
        }
    }
    labels
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probs(classes: &[u8], w: usize, h: usize) -> Probabilities {
        let n = w * h;
        let mut p = vec![0u8; 3 * n];
        for (i, &c) in classes.iter().enumerate() {
            p[c as usize * n + i] = 90;
            for k in 0..3 {
                if k != c as usize {
                    p[k * n + i] = 5;
                }
            }
        }
        Probabilities { w, h, p, dist: vec![] }
    }

    #[test]
    fn origins_cover_the_side() {
        assert_eq!(origins(100, 256, 16), vec![0]);
        assert_eq!(origins(256, 256, 16), vec![0]);
        assert_eq!(origins(500, 256, 16), vec![0, 224, 244]);
    }

    #[test]
    fn watershed_splits_touching_fields_and_takes_their_boundary() {
        // two 4x4 fields side by side, a boundary column between them, all on background
        let (w, h) = (11, 6);
        let mut c = vec![0u8; w * h];
        for y in 1..5 {
            for x in 1..10 {
                c[y * w + x] = if x == 5 || x == 1 || x == 9 || y == 1 || y == 4 { 2 } else { 1 };
            }
        }
        let p = probs(&c, w, h);
        let ftw = parcels(&p, Parcels::Interiors, 50);
        let mut ids: Vec<u32> = ftw.iter().copied().filter(|&l| l > 0).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 2, "the interiors: two parcels");
        assert_eq!(ftw.iter().filter(|&&l| l > 0).count(), 2 * 3 * 2);
        let ws = parcels(&p, Parcels::Watershed, 50);
        // every field pixel (interior or boundary) has a parcel, the two still apart
        assert!((0..w * h).all(|i| (c[i] != 0) == (ws[i] > 0)));
        let left = ws[2 * w + 2];
        let right = ws[2 * w + 8];
        assert!(left > 0 && right > 0 && left != right);
    }
}
