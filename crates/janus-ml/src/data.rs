//! What a model reads: each date's bands of a pixel (the shown band or index, source bands), read from the cube at
//! full resolution, its gaps filled in time, and each band scaled by its 2-98 % quantiles of the training samples
//! (sits' `.samples_stats` and `C_normalize_data`, clamped to 0.0001-1).

use std::sync::Arc;

use janus_cube::{CubeInfo, QaRule, with_thread_reader};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// A value a model reads on each date: the series shown ("shown": the band or normalized difference, the quality
/// band applied) or a source band of the date (1-based `band`), screened by the quality band too.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct InputBand {
    /// "shown" or "band"
    pub kind: String,
    /// the date's band, 1..=bands_per_date (0 for the shown series)
    pub band: i32,
    /// its name (the band's description, or what is shown)
    pub name: String,
}

impl InputBand {
    pub fn shown(info: &CubeInfo) -> InputBand {
        let name = info.selection_text();
        InputBand { kind: "shown".into(), band: 0, name: if name.is_empty() { "value".into() } else { name } }
    }

    pub fn source(info: &CubeInfo, band: i32) -> InputBand {
        let name = info.band_names.get(band as usize - 1).cloned().unwrap_or_else(|| format!("band {band}"));
        InputBand { kind: "band".into(), band, name }
    }

    /// The same value in another series: the shown series, or the band of the same name (else the same number).
    pub fn find_in(&self, info: &CubeInfo) -> Result<InputBand, String> {
        if self.kind == "shown" {
            let here = InputBand::shown(info);
            if here.name != self.name {
                return Err(format!(
                    "the model reads the series shown as \"{}\"; this one shows \"{}\"",
                    self.name, here.name
                ));
            }
            return Ok(here);
        }
        let by_name = info.band_names.iter().position(|n| n.eq_ignore_ascii_case(&self.name));
        match by_name {
            Some(k) => Ok(InputBand::source(info, k as i32 + 1)),
            None if self.band as usize <= info.bands_per_date && info.band_names.is_empty() => Ok(self.clone()),
            None => Err(format!("the model reads the band \"{}\", which this series does not have", self.name)),
        }
    }
}

/// The dates of a series as a model sees them.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Calendar {
    pub labels: Vec<String>,
    /// decimal years
    pub years: Vec<f64>,
    /// days since 1970 (real dates only)
    pub days: Option<Vec<i64>>,
}

impl Calendar {
    pub fn of(info: &CubeInfo) -> Calendar {
        let n = info.t();
        Calendar {
            labels: info.layers.iter().map(|l| l.label.clone()).collect(),
            years: (0..n).map(|t| info.decimal_year(t)).collect(),
            days: info.time_is_date.then(|| (0..n).map(|t| info.unix_day(t)).collect()),
        }
    }

    pub fn len(&self) -> usize {
        self.years.len()
    }

    pub fn is_empty(&self) -> bool {
        self.years.is_empty()
    }

    /// Days since the first date (sits' timeline for the positional encoding); a year of 365.25 days without
    /// real dates.
    pub fn day_offsets(&self) -> Vec<f64> {
        match &self.days {
            Some(d) if !d.is_empty() => d.iter().map(|&x| (x - d[0]) as f64).collect(),
            _ => self.years.iter().map(|&y| (y - self.years[0]) * 365.25).collect(),
        }
    }
}

/// Series of n pixels: `values[(i * t + d) * b + k]` (pixel i, date d, value k; sits' n x times x bands), NaN
/// without a value.
#[derive(Clone, Debug, Default)]
pub struct Series {
    pub n: usize,
    pub t: usize,
    pub b: usize,
    pub values: Vec<f32>,
}

impl Series {
    pub fn pixel(&self, i: usize) -> &[f32] {
        &self.values[i * self.t * self.b..(i + 1) * self.t * self.b]
    }

    /// The pixels `keep` of it.
    pub fn select(&self, keep: &[usize]) -> Series {
        let len = self.t * self.b;
        let mut values = Vec::with_capacity(keep.len() * len);
        for &i in keep {
            values.extend_from_slice(self.pixel(i));
        }
        Series { n: keep.len(), t: self.t, b: self.b, values }
    }
}

/// The inputs of every date over a window (x, y, w, h) of the cube, read on several threads (one reader each):
/// [date][input][w * h].
fn read_dates(info: &Arc<CubeInfo>, inputs: &[InputBand], win: (usize, usize, usize, usize)) -> Vec<Vec<f32>> {
    let n = win.2 * win.3;
    let sel = info.sel;
    (0..info.t())
        .into_par_iter()
        .map(|t| {
            with_thread_reader(info, |r| {
                let mut out = vec![f32::NAN; inputs.len() * n];
                let mut qa = vec![];
                if sel.qa_band > 0 && sel.qa_rule != QaRule::None && inputs.iter().any(|b| b.kind != "shown") {
                    qa = vec![f32::NAN; n];
                    if !r.read_band_window(t, sel.qa_band, win, &mut qa, (win.2, win.3)) {
                        qa.fill(f32::NAN);
                    }
                }
                for (k, b) in inputs.iter().enumerate() {
                    let buf = &mut out[k * n..(k + 1) * n];
                    let ok = if b.kind == "shown" {
                        r.read_window(t, win, buf, (win.2, win.3))
                    } else {
                        r.read_band_window(t, b.band, win, buf, (win.2, win.3))
                    };
                    if !ok {
                        buf.fill(f32::NAN);
                    } else if b.kind != "shown" && !qa.is_empty() {
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
        .collect()
}

/// The series of the pixels of a window (row by row).
pub fn read_window(info: &Arc<CubeInfo>, inputs: &[InputBand], win: (usize, usize, usize, usize)) -> Series {
    let dates = read_dates(info, inputs, win);
    let (n, t, b) = (win.2 * win.3, info.t(), inputs.len());
    let mut values = vec![f32::NAN; n * t * b];
    values.par_chunks_mut(t * b).enumerate().for_each(|(i, px)| {
        for (d, date) in dates.iter().enumerate() {
            for k in 0..b {
                px[d * b + k] = date[k * n + i];
            }
        }
    });
    Series { n, t, b, values }
}

/// The series of single pixels (samples), in their order.
pub fn read_points(info: &Arc<CubeInfo>, inputs: &[InputBand], points: &[(usize, usize)]) -> Series {
    let (t, b) = (info.t(), inputs.len());
    let mut values = vec![f32::NAN; points.len() * t * b];
    // date by date (a reader per thread), every point of the date
    let per_date: Vec<Vec<f32>> = (0..t)
        .into_par_iter()
        .map(|d| {
            let mut out = vec![f32::NAN; points.len() * b];
            for (i, &(x, y)) in points.iter().enumerate() {
                if x < info.width && y < info.height {
                    let one = read_dates_at(info, inputs, d, x, y);
                    out[i * b..(i + 1) * b].copy_from_slice(&one);
                }
            }
            out
        })
        .collect();
    for (d, date) in per_date.iter().enumerate() {
        for i in 0..points.len() {
            for k in 0..b {
                values[(i * t + d) * b + k] = date[i * b + k];
            }
        }
    }
    Series { n: points.len(), t, b, values }
}

/// One date's inputs at a pixel.
fn read_dates_at(info: &Arc<CubeInfo>, inputs: &[InputBand], t: usize, x: usize, y: usize) -> Vec<f32> {
    let sel = info.sel;
    with_thread_reader(info, |r| {
        let masked = sel.qa_band > 0
            && sel.qa_rule != QaRule::None
            && r.read_band_pixel(t, sel.qa_band, x, y, true)
                .is_none_or(|q| q.is_nan() || q < 0.0 || !sel.qa_rule.usable(q as u32));
        inputs
            .iter()
            .map(|b| {
                let v =
                    if b.kind == "shown" { r.read_pixel(t, x, y) } else { r.read_band_pixel(t, b.band, x, y, false) };
                match v {
                    Some(v) if v.is_finite() && !(b.kind != "shown" && masked) => v,
                    _ => f32::NAN,
                }
            })
            .collect()
    })
}

/// Each pixel's gaps filled linearly in time between its dates with a value (the first and last values repeated
/// at the ends), each value on its own; a pixel with fewer than `min_valid` % of its dates with a value (in any of
/// its values) is left out: its values NaN and false in the result.
pub fn fill_gaps(s: &mut Series, years: &[f64], min_valid: f64) -> Vec<bool> {
    let (t, b) = (s.t, s.b);
    let need = ((min_valid / 100.0 * t as f64).ceil() as usize).clamp(1, t.max(1));
    s.values
        .par_chunks_mut(t * b)
        .map(|px| {
            let mut ok = true;
            for k in 0..b {
                let have: Vec<usize> = (0..t).filter(|&d| px[d * b + k].is_finite()).collect();
                if have.len() < need || have.is_empty() {
                    ok = false;
                    break;
                }
                let mut j = 0; // the last date with a value at or before d
                for d in 0..t {
                    while j + 1 < have.len() && have[j + 1] <= d {
                        j += 1;
                    }
                    if px[d * b + k].is_finite() {
                        continue;
                    }
                    let (a, c) = if d < have[0] {
                        (have[0], have[0])
                    } else if j + 1 < have.len() {
                        (have[j], have[j + 1])
                    } else {
                        (have[j], have[j])
                    };
                    let (va, vc) = (px[a * b + k] as f64, px[c * b + k] as f64);
                    let f = if years[c] > years[a] { (years[d] - years[a]) / (years[c] - years[a]) } else { 0.0 };
                    px[d * b + k] = (va + (vc - va) * f) as f32;
                }
            }
            if !ok {
                px.fill(f32::NAN);
            }
            ok
        })
        .collect()
}

/// R's quantile (type 7) of sorted values.
fn quantile7(sorted: &[f32], p: f64) -> f32 {
    if sorted.is_empty() {
        return f32::NAN;
    }
    let h = (sorted.len() - 1) as f64 * p;
    let (lo, hi) = (h.floor() as usize, h.ceil() as usize);
    let f = h - lo as f64;
    (sorted[lo] as f64 + (sorted[hi] as f64 - sorted[lo] as f64) * f) as f32
}

/// The scaling of each value: its 2 % and 98 % quantiles over the training samples.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Norm {
    pub q02: Vec<f32>,
    pub q98: Vec<f32>,
}

impl Norm {
    pub fn fit(s: &Series) -> Norm {
        let (mut q02, mut q98) = (vec![], vec![]);
        for k in 0..s.b {
            let mut v: Vec<f32> = s.values.iter().skip(k).step_by(s.b).copied().filter(|x| x.is_finite()).collect();
            v.sort_by(f32::total_cmp);
            q02.push(quantile7(&v, 0.02));
            q98.push(quantile7(&v, 0.98));
        }
        Norm { q02, q98 }
    }

    /// (x - q02) / (q98 - q02), clamped to 0.0001-1 (sits' C_normalize_data); NaN stays NaN.
    pub fn apply(&self, s: &mut Series) {
        let b = s.b;
        s.values.par_chunks_mut(b).for_each(|v| {
            for (k, x) in v.iter_mut().enumerate() {
                let d = self.q98[k] - self.q02[k];
                if x.is_finite() {
                    *x = ((*x - self.q02[k]) / if d != 0.0 { d } else { 1.0 }).clamp(0.0001, 1.0);
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gaps_are_filled_in_time() {
        let mut s = Series {
            n: 2,
            t: 5,
            b: 1,
            values: vec![f32::NAN, 1.0, f32::NAN, 3.0, f32::NAN, 1.0, f32::NAN, f32::NAN, f32::NAN, f32::NAN],
        };
        let keep = fill_gaps(&mut s, &[0.0, 1.0, 2.0, 3.0, 4.0], 40.0);
        assert_eq!(keep, vec![true, false]);
        assert_eq!(&s.values[..5], &[1.0, 1.0, 2.0, 3.0, 3.0]);
        assert!(s.values[5..].iter().all(|v| v.is_nan()));
    }

    #[test]
    fn quantiles_are_rs() {
        let v: Vec<f32> = (1..=11).map(|x| x as f32).collect();
        // R: quantile(1:11, c(0.02, 0.98)) = 1.2, 10.8
        assert!((quantile7(&v, 0.02) - 1.2).abs() < 1e-6);
        assert!((quantile7(&v, 0.98) - 10.8).abs() < 1e-6);
    }
}
