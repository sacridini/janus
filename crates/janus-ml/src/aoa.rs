//! The area of applicability of a model (Meyer & Pebesma 2021), as Zeit's `aoa` (CAST's `trainDI` and `aoa`): the
//! training samples' series, as the network reads them (gaps filled, scaled), kept in the model with the threshold
//! of their dissimilarity to each other; a pixel's dissimilarity index (DI) is its distance to the nearest sample
//! over their mean distance, and it is in the area of applicability when that is not above the threshold. A
//! sample's own DI is to the nearest sample of another place (a polygon, a pin), as the model would meet a new place.
//! The networks have no importance of their values: they weigh alike; a Random Forest's weighs each by its
//! importance (CAST's weights from the model's `varImp`).

use serde::{Deserialize, Serialize};
use zeit_core::aoa::TrainDi;

use crate::data::Series;
use crate::train::Rng;

/// The values kept at most (samples x values): about 4 MB in the model.
const MAX_VALUES: usize = 1_000_000;

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
        let p = x.t * x.b;
        let weights = match weights {
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
        let td = zeit_core::aoa::train_di(&values, idx.len(), p, &weights, folds.as_deref(), n_jobs)?;
        if !td.threshold.is_finite() {
            return Err("the samples' dissimilarity has no threshold".into());
        }
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
        Ok(TrainDi {
            n: self.points,
            p,
            center: self.center.clone(),
            scale: self.scale.clone(),
            weight: self.weight.clone(),
            train,
            mean_distance: self.mean_distance,
            train_di: vec![],
            threshold: self.threshold,
        })
    }

    pub fn describe(&self) -> String {
        format!(
            "{} samples{}, DI threshold {:.3} ({})",
            self.points,
            if self.points < self.of { format!(" of {}", self.of) } else { String::new() },
            self.threshold,
            if self.folds == "places" { "to other places" } else { "to other samples" }
        )
    }
}

/// The DI of n pixels' inputs (n x values, as the network reads them).
pub fn di(x: &[f32], n: usize, td: &TrainDi, n_jobs: i32) -> Vec<f64> {
    let values: Vec<f64> = x.iter().map(|&v| v as f64).collect();
    zeit_core::aoa::di(&values, n, td, false, n_jobs).0
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
        let z = zeit_core::aoa::train_di(&v, n, t * b, &[1.0; 6], Some(&f), 1).unwrap();
        assert!((z.threshold - a.threshold).abs() < 1e-12 && (z.mean_distance - a.mean_distance).abs() < 1e-12);
        // samples that do not fit the values
        let a = Applicability { train: String::new(), ..a };
        assert!(a.train_di().is_err());
    }
}
