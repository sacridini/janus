//! The area of applicability of a model (Meyer & Pebesma 2021), as Zeit's `aoa` (CAST's `trainDI` and `aoa`): the
//! training samples' series, as the network reads them (gaps filled, scaled), kept in the model with the threshold
//! of their dissimilarity to each other; a pixel's dissimilarity index (DI) is its distance to the nearest sample
//! over their mean distance, and it is in the area of applicability when that is not above the threshold. A
//! sample's own DI is to the nearest sample of another place (a polygon, a pin), as the model would meet a new place.
//! The networks have no importance of their values: they weigh alike; a Random Forest's weighs each by its
//! importance (CAST's weights from the model's `varImp`). The distance is Euclidean, or Mahalanobis (the samples
//! whitened by their covariance, CAST's `method = "MD"`).
//!
//! The model's error against the DI (CAST's `errorProfiles`, Zeit's `error_profile`): the validation samples (whole
//! places held out from the training) sorted by their DI to the samples of the other places, the accuracy in moving
//! windows of `WINDOW` of them, a curve that only falls fitted to those (zeit-core's monotone P-spline); applied to a
//! pixel's DI, the accuracy to expect there, inside the AOA. A pixel's local point density (LPD) is how many samples
//! are that near (its DI to them under the threshold).

use serde::{Deserialize, Serialize};
use zeit_core::aoa::TrainDi;

use crate::data::Series;
use crate::train::Rng;

/// The values kept at most (samples x values): about 4 MB in the model.
const MAX_VALUES: usize = 1_000_000;

/// The error profile's moving window (CAST's `window.size = 5`) and B-splines (its `k = 6`).
const WINDOW: usize = 5;
const SPLINES: usize = 6;

/// How a model's accuracy falls with the dissimilarity of the data to its samples (CAST's `errorProfiles`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Profile {
    /// "accuracy" (of classes)
    pub metric: String,
    pub window: usize,
    /// the validation samples it was fitted from (those within the threshold)
    pub samples: usize,
    /// each window's DI (its middle sample's) and accuracy
    pub points: Vec<[f64; 2]>,
    /// the monotone curve: its knots' range, its B-splines' coefficients, its smoothness and degrees of freedom
    pub lo: f64,
    pub hi: f64,
    pub coef: Vec<f64>,
    pub lambda: f64,
    pub edf: f64,
}

impl Profile {
    fn curve(&self) -> zeit_core::profile::Monotone {
        zeit_core::profile::Monotone {
            lo: self.lo,
            hi: self.hi,
            coef: self.coef.clone(),
            lambda: self.lambda,
            edf: self.edf,
        }
    }

    /// The accuracy to expect at each DI (0 to 1; NaN where the DI is).
    pub fn at(&self, di: &[f64]) -> Vec<f64> {
        let c = self.curve();
        di.iter().map(|&d| if d.is_finite() { c.at(d).clamp(0.0, 1.0) } else { f64::NAN }).collect()
    }

    /// The validation samples (sample index, class given) of the DI `di` (of the samples `idx`) within the threshold
    /// sorted by it, the accuracy in moving windows, the curve; None with too few.
    fn fit(idx: &[usize], di: &[f64], threshold: f64, y: &[u32], held_out: &[(usize, u32)]) -> Option<Profile> {
        let given: std::collections::HashMap<usize, u32> = held_out.iter().copied().collect();
        let mut rows: Vec<(f64, bool)> = idx
            .iter()
            .zip(di)
            .filter(|(_, d)| d.is_finite() && **d <= threshold)
            .filter_map(|(&i, &d)| given.get(&i).map(|&c| (d, c == y[i])))
            .collect();
        if rows.len() < WINDOW + 3 {
            return None;
        }
        rows.sort_by(|a, b| a.0.total_cmp(&b.0));
        let middle = (WINDOW - 1) / 2;
        let points: Vec<[f64; 2]> = rows
            .windows(WINDOW)
            .map(|w| [w[middle].0, w.iter().filter(|r| r.1).count() as f64 / WINDOW as f64])
            .collect();
        let (x, acc): (Vec<f64>, Vec<f64>) = points.iter().map(|p| (p[0], p[1])).unzip();
        let m = zeit_core::profile::fit_monotone(&x, &acc, false, SPLINES)?;
        Some(Profile {
            metric: "accuracy".into(),
            window: WINDOW,
            samples: rows.len(),
            points,
            lo: m.lo,
            hi: m.hi,
            coef: m.coef,
            lambda: m.lambda,
            edf: m.edf,
        })
    }
}

/// How the area of applicability is fitted besides the samples: each value's weight (its importance; `None`:
/// alike), the validation samples and the classes the model gave them (for the error profile), the distance.
#[derive(Clone, Copy, Debug, Default)]
pub struct Fit<'a> {
    pub weights: Option<&'a [f64]>,
    pub held_out: Option<&'a [(usize, u32)]>,
    pub mahalanobis: bool,
}

/// What a model keeps of its samples for its area of applicability.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Applicability {
    /// each value's mean and deviation over the samples, and its weight
    pub center: Vec<f64>,
    pub scale: Vec<f64>,
    pub weight: Vec<f64>,
    pub mean_distance: f64,
    pub threshold: f64,
    /// the samples kept (of `of`), their DI's folds ("places" or "samples")
    pub points: usize,
    pub of: usize,
    pub folds: String,
    /// the samples scaled and weighted (points x values), float32 little-endian in base64
    pub train: String,
    /// "euclidean" or "mahalanobis" (what whitens a scaled and weighted point: values x values, lower triangular,
    /// float32 in base64)
    #[serde(default = "euclidean", skip_serializing_if = "is_euclidean")]
    pub distance: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub whiten: Option<String>,
    /// the error profile (models trained with their validation by places)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<Profile>,
}

fn euclidean() -> String {
    "euclidean".into()
}

fn is_euclidean(s: &String) -> bool {
    s == "euclidean"
}

impl Applicability {
    /// From the samples as the network reads them, each one's place (`None`: each its own) and class; at most
    /// MAX_VALUES values kept, as many of each class as their share.
    pub fn fit(x: &Series, places: Option<&[usize]>, y: &[u32], n_jobs: i32) -> Result<Applicability, String> {
        Applicability::fit_weighted(x, places, y, None, n_jobs)
    }

    /// As `fit`, each value weighed (its importance; `None`: alike).
    pub fn fit_weighted(
        x: &Series,
        places: Option<&[usize]>,
        y: &[u32],
        weights: Option<&[f64]>,
        n_jobs: i32,
    ) -> Result<Applicability, String> {
        Applicability::fit_with(x, places, y, Fit { weights, ..Fit::default() }, n_jobs)
    }

    /// As `fit`, with the weights, the error profile (from the validation samples and the classes the model gave
    /// them) and the distance of `how`.
    pub fn fit_with(
        x: &Series,
        places: Option<&[usize]>,
        y: &[u32],
        how: Fit,
        n_jobs: i32,
    ) -> Result<Applicability, String> {
        let p = x.t * x.b;
        let weights = match how.weights {
            Some(w) if w.len() == p => w.to_vec(),
            Some(_) => return Err("a weight for each value is needed".into()),
            None => vec![1.0; p],
        };
        let ok: Vec<usize> = (0..x.n).filter(|&i| x.pixel(i).iter().all(|v| v.is_finite())).collect();
        let cap = (MAX_VALUES / p.max(1)).clamp(100, 5000);
        let idx = if ok.len() > cap {
            let mut rng = Rng::new(7);
            let mut idx = vec![];
            let classes = y.iter().copied().max().map_or(0, |m| m as usize + 1);
            for c in 0..classes as u32 {
                let mut of: Vec<usize> = ok.iter().copied().filter(|&i| y[i] == c).collect();
                rng.shuffle(&mut of);
                let take = ((of.len() * cap) as f64 / ok.len() as f64).round().max(1.0) as usize;
                idx.extend_from_slice(&of[..take.min(of.len())]);
            }
            idx.sort_unstable();
            idx
        } else {
            ok
        };
        if idx.len() < 3 {
            return Err("fewer than 3 samples with every value".into());
        }
        let values: Vec<f64> = idx.iter().flat_map(|&i| x.pixel(i).iter().map(|&v| v as f64)).collect();
        let folds: Option<Vec<i64>> = places
            .filter(|pl| idx.iter().map(|&i| pl[i]).collect::<std::collections::BTreeSet<_>>().len() >= 2)
            .map(|pl| idx.iter().map(|&i| pl[i] as i64).collect());
        let td = zeit_core::aoa::train_di(
            &values,
            idx.len(),
            p,
            &weights,
            folds.as_deref(),
            None,
            if how.mahalanobis { zeit_core::aoa::Distance::Mahalanobis } else { zeit_core::aoa::Distance::Euclidean },
            n_jobs,
        )?;
        if !td.threshold.is_finite() {
            return Err("the samples' dissimilarity has no threshold".into());
        }
        // the error profile: the samples' own DI is to other places, as a validation by places held them out
        let profile = match (how.held_out, &folds) {
            (Some(h), Some(_)) => Profile::fit(&idx, &td.train_di, td.threshold, y, h),
            _ => None,
        };
        let f32s = |v: &[f64]| -> String {
            let bytes: Vec<u8> = v.iter().flat_map(|&v| (v as f32).to_le_bytes()).collect();
            encode(&bytes)
        };
        let bytes: Vec<u8> = td.train.iter().flat_map(|&v| (v as f32).to_le_bytes()).collect();
        Ok(Applicability {
            center: td.center,
            scale: td.scale,
            weight: td.weight,
            mean_distance: td.mean_distance,
            threshold: td.threshold,
            points: idx.len(),
            of: x.n,
            folds: if folds.is_some() { "places" } else { "samples" }.into(),
            train: encode(&bytes),
            distance: if how.mahalanobis { "mahalanobis" } else { "euclidean" }.into(),
            whiten: td.whiten.as_deref().map(f32s),
            profile,
        })
    }

    /// The training data to measure pixels against.
    pub fn train_di(&self) -> Result<TrainDi, String> {
        let bytes = decode(&self.train).ok_or("the model's samples are damaged")?;
        let p = self.center.len();
        let train: Vec<f64> = bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b) as f64).collect();
        if p == 0 || train.len() != self.points * p || self.scale.len() != p || self.weight.len() != p {
            return Err("the model's samples do not fit its values".into());
        }
        let whiten = match &self.whiten {
            Some(w) => {
                let bytes = decode(w).ok_or("the model's whitening is damaged")?;
                let w: Vec<f64> = bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b) as f64).collect();
                if w.len() != p * p {
                    return Err("the model's whitening does not fit its values".into());
                }
                Some(w)
            }
            None => None,
        };
        Ok(TrainDi {
            n: self.points,
            p,
            center: self.center.clone(),
            scale: self.scale.clone(),
            weight: self.weight.clone(),
            whiten,
            train,
            mean_distance: self.mean_distance,
            train_di: vec![],
            threshold: self.threshold,
        })
    }

    pub fn describe(&self) -> String {
        let mut s = format!(
            "{} samples{}, DI threshold {:.3} ({}{})",
            self.points,
            if self.points < self.of { format!(" of {}", self.of) } else { String::new() },
            self.threshold,
            if self.folds == "places" { "to other places" } else { "to other samples" },
            if self.distance == "mahalanobis" { ", Mahalanobis distance" } else { "" }
        );
        if let Some(p) = &self.profile {
            let a = p.at(&[0.0, self.threshold]);
            s += &format!(
                "; expected accuracy {:.0}% at DI 0 to {:.0}% at the threshold (from {} validation samples)",
                100.0 * a[0],
                100.0 * a[1],
                p.samples
            );
        }
        s
    }
}

/// The DI of n pixels' inputs (n x values, as the network reads them).
pub fn di(x: &[f32], n: usize, td: &TrainDi, n_jobs: i32) -> Vec<f64> {
    di_lpd(x, n, td, false, n_jobs).0
}

/// The DI of n pixels' inputs and, with `lpd`, their local point density (how many samples are that near; else
/// empty).
pub fn di_lpd(x: &[f32], n: usize, td: &TrainDi, lpd: bool, n_jobs: i32) -> (Vec<f64>, Vec<f64>) {
    let values: Vec<f64> = x.iter().map(|&v| v as f64).collect();
    zeit_core::aoa::di(&values, n, td, lpd, n_jobs)
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let v = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for k in 0..4 {
            out.push(if k <= c.len() { B64[(v >> (18 - 6 * k) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

fn decode(s: &str) -> Option<Vec<u8>> {
    let s = s.as_bytes();
    if !s.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    for c in s.chunks(4) {
        let mut v = 0u32;
        let mut pad = 0;
        for &b in c {
            let d = match b {
                b'A'..=b'Z' => b - b'A',
                b'a'..=b'z' => b - b'a' + 26,
                b'0'..=b'9' => b - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                b'=' => {
                    pad += 1;
                    0
                }
                _ => return None,
            };
            v = v << 6 | d as u32;
        }
        out.extend_from_slice(&v.to_be_bytes()[1..4 - pad]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trip() {
        for n in 0..10 {
            let bytes: Vec<u8> = (0..n).map(|i| (i * 37 + 200) as u8).collect();
            assert_eq!(decode(&encode(&bytes)).unwrap(), bytes);
        }
        assert_eq!(encode(b"Man"), "TWFu");
        assert_eq!(encode(b"Ma"), "TWE=");
    }

    #[test]
    fn fit_and_measure() {
        // two places of samples around 0 and 1 in 3 dates x 2 values; a pixel among them inside, one far outside
        let mut rng = Rng::new(1);
        let mut u = || (rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64 - 0.5;
        let (n, t, b) = (40, 3, 2);
        let values: Vec<f32> =
            (0..n).flat_map(|i| (0..t * b).map(|_| (i % 2) as f32 + 0.2 * u() as f32).collect::<Vec<_>>()).collect();
        let x = Series { n, t, b, values };
        let places: Vec<usize> = (0..n).map(|i| i / 10).collect();
        let y: Vec<u32> = (0..n).map(|i| (i % 2) as u32).collect();
        let a = Applicability::fit(&x, Some(&places), &y, 1).unwrap();
        assert_eq!((a.points, a.folds.as_str()), (40, "places"));
        let td = a.train_di().unwrap();
        let px = [x.pixel(3).to_vec(), vec![5.0f32; 6]].concat();
        let d = di(&px, 2, &td, 1);
        assert!(d[0] < 1e-6 && d[1] > td.threshold);
        // the same as Zeit's on the same values
        let v: Vec<f64> = x.values.iter().map(|&v| v as f64).collect();
        let f: Vec<i64> = places.iter().map(|&p| p as i64).collect();
        let z =
            zeit_core::aoa::train_di(&v, n, t * b, &[1.0; 6], Some(&f), None, zeit_core::aoa::Distance::Euclidean, 1)
                .unwrap();
        assert!((z.threshold - a.threshold).abs() < 1e-12 && (z.mean_distance - a.mean_distance).abs() < 1e-12);
        // samples that do not fit the values
        let a = Applicability { train: String::new(), ..a };
        assert!(a.train_di().is_err());
        // the Mahalanobis distance: its whitening kept and read back, the DI as Zeit's
        let how = Fit { mahalanobis: true, ..Fit::default() };
        let m = Applicability::fit_with(&x, Some(&places), &y, how, 1).unwrap();
        let back: Applicability = serde_json::from_value(serde_json::to_value(&m).unwrap()).unwrap();
        assert_eq!(back, m);
        assert_eq!((m.distance.as_str(), m.whiten.is_some()), ("mahalanobis", true));
        let z =
            zeit_core::aoa::train_di(&v, n, t * b, &[1.0; 6], Some(&f), None, zeit_core::aoa::Distance::Mahalanobis, 1)
                .unwrap();
        assert!((z.threshold - m.threshold).abs() < 1e-9);
        let (ours, theirs) = (
            di(&px, 2, &back.train_di().unwrap(), 1),
            zeit_core::aoa::di(&px.iter().map(|&v| v as f64).collect::<Vec<_>>(), 2, &z, false, 1).0,
        );
        for (o, t) in ours.iter().zip(&theirs) {
            assert!((o - t).abs() < 1e-4 * t.max(1.0), "Mahalanobis DI {o} vs Zeit's {t}");
        }
    }

    #[test]
    fn error_profile_falls_with_the_dissimilarity() {
        // validation samples right near the others, wrong far from them: a curve that only falls
        let idx: Vec<usize> = (0..40).collect();
        let di: Vec<f64> = (0..40).map(|i| i as f64 / 40.0).collect();
        let y = vec![0u32; 40];
        let held_out: Vec<(usize, u32)> = (0..40).map(|i| (i, u32::from(i >= 25))).collect();
        let p = Profile::fit(&idx, &di, 1.0, &y, &held_out).unwrap();
        assert_eq!((p.samples, p.points.len()), (40, 36));
        let a = p.at(&[0.0, 0.3, 0.6, 0.97, f64::NAN]);
        assert!(a[0] > 0.9 && a[3] < 0.1, "{a:?}");
        assert!(a[0] >= a[1] && a[1] >= a[2] && a[2] >= a[3] && a[4].is_nan());
        // beyond the threshold left out; too few for a window
        assert_eq!(Profile::fit(&idx, &di, 0.5, &y, &held_out).unwrap().samples, 21);
        assert!(Profile::fit(&idx[..7], &di[..7], 1.0, &y, &held_out).is_none());
    }
}
