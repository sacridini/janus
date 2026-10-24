//! The class map's post-processing after a model is applied, as Zeit's: sits' Bayesian smoothing of the
//! probabilities (zeit.smooth_probs, sits_smooth), then a minimum mapping unit that merges the small patches into the
//! neighbouring class of the longest border (zeit.mmu_sieve).

use zeit_core::postproc;

/// What is done to the class map after the model (nothing by default).
#[derive(Clone, Debug, PartialEq)]
pub struct PostProcess {
    /// sits' Bayesian smoothing of the probabilities: its window (odd), smoothness and fraction of the neighbours
    pub smooth: bool,
    pub window: usize,
    pub smoothness: f64,
    pub neigh_fraction: f64,
    /// patches of fewer pixels merged into their neighbours, 8-connected (0: off)
    pub min_size: usize,
}

impl Default for PostProcess {
    fn default() -> Self {
        // sits_smooth's defaults
        PostProcess { smooth: false, window: 9, smoothness: 20.0, neigh_fraction: 0.5, min_size: 0 }
    }
}

impl PostProcess {
    pub fn on(&self) -> bool {
        self.smooth || self.min_size >= 2
    }

    /// As the log and the outputs say it.
    pub fn describe(&self) -> String {
        let mut parts = vec![];
        if self.smooth {
            parts.push(format!(
                "Bayesian smoothing (window {}, smoothness {}, neighbours {})",
                self.window, self.smoothness, self.neigh_fraction
            ));
        }
        if self.min_size >= 2 {
            parts.push(format!("minimum mapping unit {} px", self.min_size));
        }
        parts.join(", ")
    }
}

/// The class (1-based, 0 without one) and its smoothed probability of each pixel, from the probabilities `probs`
/// (pixel-major, `k` each, NaN where not classified) of a `w` x `h` map: smoothed in bands of rows with the window's
/// overlap, which gives what the whole map at once would.
pub fn smooth(probs: &[f32], w: usize, h: usize, k: usize, p: &PostProcess) -> (Vec<u8>, Vec<f32>) {
    let window = p.window.max(3) | 1;
    let leg = window / 2;
    let smoothness = vec![p.smoothness; k];
    let (mut cls, mut best) = (vec![0u8; w * h], vec![f32::NAN; w * h]);
    // about 256 MB a band: the probabilities, their logits and the result as float64
    let rows = ((256usize << 20) / (w.max(1) * k.max(1) * 24)).max(32);
    let mut r0 = 0;
    while r0 < h {
        let r1 = (r0 + rows).min(h);
        let (a, b) = (r0.saturating_sub(leg), (r1 + leg).min(h));
        let n = (b - a) * w;
        let mut block = vec![f64::NAN; k * n];
        for (i, px) in (a * w..b * w).enumerate() {
            for c in 0..k {
                block[c * n + i] = f64::from(probs[px * k + c]);
            }
        }
        let s = postproc::smooth_probs(&block, k, b - a, w, window, &smoothness, p.neigh_fraction, -1);
        for px in r0 * w..r1 * w {
            let i = px - a * w;
            let (mut top, mut at) = (f64::NEG_INFINITY, 0);
            for c in 0..k {
                let v = s[c * n + i];
                if v > top {
                    (top, at) = (v, c + 1);
                }
            }
            if at > 0 {
                (cls[px], best[px]) = (at as u8, top as f32);
            }
        }
        r0 = r1;
    }
    (cls, best)
}

/// The patches of `cls` (0: no class) under `min_size` pixels merged into their neighbours, 8-connected.
pub fn sieve(cls: &mut [u8], w: usize, h: usize, min_size: usize) {
    let map: Vec<f64> = cls.iter().map(|&c| if c == 0 { f64::NAN } else { f64::from(c) }).collect();
    let out = postproc::sieve(&map, h, w, min_size, true);
    for (c, v) in cls.iter_mut().zip(out) {
        *c = if v.is_nan() { 0 } else { v as u8 };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoothing_in_bands_is_smoothing_the_whole_map() {
        let (w, h, k) = (13, 70, 3);
        let mut seed = 5u64;
        let mut probs = vec![0f32; w * h * k];
        for px in 0..w * h {
            let mut s = [0f32; 3];
            for v in &mut s {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                *v = ((seed >> 33) % 1000) as f32 / 1000.0 + if px % w < 6 { 0.5 } else { 0.0 };
            }
            let sum: f32 = s.iter().sum();
            for c in 0..k {
                probs[px * k + c] = s[c] / sum;
            }
        }
        probs[40 * k..41 * k].fill(f32::NAN);
        let p = PostProcess { smooth: true, window: 5, ..Default::default() };
        // the whole map at once
        let n = w * h;
        let block: Vec<f64> = (0..k * n).map(|i| f64::from(probs[(i % n) * k + i / n])).collect();
        let s = postproc::smooth_probs(&block, k, h, w, 5, &[20.0; 3], 0.5, 1);
        let (cls, best) = smooth(&probs, w, h, k, &p);
        assert_eq!((cls[40], best[40].is_nan()), (0, true));
        for px in (0..n).filter(|&px| px != 40) {
            let (c, v) =
                (1..=k).map(|c| (c, s[(c - 1) * n + px])).fold((0, f64::MIN), |a, b| if b.1 > a.1 { b } else { a });
            assert_eq!((cls[px] as usize, best[px]), (c, v as f32), "pixel {px}");
        }
    }

    #[test]
    fn sieve_keeps_unclassified_pixels() {
        let mut cls = vec![1u8; 25];
        cls[12] = 2;
        cls[0] = 0;
        sieve(&mut cls, 5, 5, 3);
        assert_eq!((cls[12], cls[0]), (1, 0));
    }
}
