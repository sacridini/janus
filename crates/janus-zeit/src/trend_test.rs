//! Analysis > Trend test: PARTS (remotePARTS's fitGLS_partition, Ives et al. 2021) over the pixels of a region.
//! Each pixel's trend by AR(1) REML (the AR(1) trend tool's fit), the spatial correlation of the pixels fitted to
//! the correlations of their residual series, then the GLS of the trends on the design (a mean, or a mean per
//! class) in random partitions of the pixels, combined into a t-test of each mean and a chi-square test that the
//! classes differ. Counting the pixels whose own test is significant is no regional test: neighbours are
//! correlated, and thousands of tests are run at once; the counts are given beside PARTS's answer to show it.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};

use zeit_core::ar_reml;
pub use zeit_core::ar_reml::Gaps;
use zeit_core::gls::Nugget;
use zeit_core::parts;
use zeit_core::spatial::{self, Correlation};

use crate::pixel::PixelInput;

/// The pixels tested.
#[derive(Clone, Debug, Default)]
pub struct Pixels {
    /// each pixel's series (pixels x dates, row-major), NaN missing
    pub values: Vec<f64>,
    /// the decimal year of each date
    pub years: Vec<f64>,
    /// each pixel's place, in the units distances are measured in (a projected CRS's)
    pub x: Vec<f64>,
    pub y: Vec<f64>,
    /// each pixel's class (None: no class, the pixel is left out); None: no classes, the region's mean
    pub classes: Option<Vec<Option<i32>>>,
}

#[derive(Clone, Debug)]
pub struct Settings {
    /// the trend per year of the dates (else per time step)
    pub per_year: bool,
    pub harmonics: usize,
    pub gaps: Gaps,
    pub partsize: usize,
    pub max_partitions: usize,
    pub ncross: usize,
    /// None: each partition's maximum-likelihood nugget
    pub nugget: Option<f64>,
    /// pixels sampled to fit the spatial correlation
    pub fit_n: usize,
    pub seed: u64,
    pub alpha: f64,
    pub n_jobs: i32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            per_year: true,
            harmonics: 0,
            gaps: Gaps::Exact,
            partsize: 1500,
            max_partitions: 100,
            ncross: 6,
            nugget: None,
            fit_n: 1000,
            seed: 1,
            alpha: 0.05,
            n_jobs: -1,
        }
    }
}

/// A mean of the design: the region's, or a class's.
#[derive(Clone, Debug, PartialEq)]
pub struct Mean {
    pub class: Option<i32>,
    pub pixels: usize,
    /// PARTS: the estimate and its correlated t-test
    pub estimate: f64,
    pub se: f64,
    pub t: f64,
    pub p: f64,
    /// the pixels' own tests: how many have a significant increasing or decreasing trend
    pub significant_up: usize,
    pub significant_down: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    pub means: Vec<Mean>,
    /// classes left out, with too few pixels for every partition to have some
    pub left_out: Vec<(i32, usize)>,
    /// the chi-square test that the classes' means differ
    pub p_chisqr: Option<f64>,
    pub f: f64,
    pub df1: usize,
    pub df2: usize,
    /// the exponential correlation's range, in the units of `x` and `y`
    pub range: f64,
    /// the partitions' nuggets: their mean
    pub nugget: f64,
    pub npart: usize,
    pub partsize: usize,
    /// pixels with a trend (and a class), and those tested (in the partitions)
    pub fitted: usize,
    pub tested: usize,
}

/// Runs the test; `progress(fraction, stage)`, `cancel` between the stages (an error "cancelled").
pub fn run(px: &Pixels, set: &Settings, progress: &dyn Fn(f64, &str), cancel: &AtomicBool) -> Result<Outcome, String> {
    let nt = px.years.len();
    let n_all = px.x.len();
    if nt < 4 || px.values.len() != n_all * nt || px.y.len() != n_all {
        return Err("the pixels' series and places do not match".into());
    }
    let stop = || if cancel.load(Ordering::Relaxed) { Err("cancelled".to_string()) } else { Ok(()) };
    let input = PixelInput { years: px.years.clone(), ..Default::default() };
    let (design, q) = crate::ar_trend::design_of(&input, set.per_year, set.harmonics);

    // the pixels' trends
    progress(0.0, "Fitting each pixel's trend");
    let out = ar_reml::batch(&px.values, n_all, nt, &design, q, set.gaps, q + 2, set.n_jobs);
    let (slope, pv) = (&out[n_all..2 * n_all], &out[(3 * q + 1) * n_all..(3 * q + 2) * n_all]);
    stop()?;
    // the pixels with a trend and a class; classes too small for every partition left out
    let class_of = |i: usize| px.classes.as_ref().map(|c| c[i]);
    let mut ok: Vec<usize> = (0..n_all).filter(|&i| slope[i].is_finite() && class_of(i) != Some(None)).collect();
    let fitted = ok.len();
    let mut left_out = vec![];
    if px.classes.is_some() {
        let mut counts: BTreeMap<i32, usize> = BTreeMap::new();
        for &i in &ok {
            *counts.entry(class_of(i).flatten().unwrap()).or_default() += 1;
        }
        // a class needs about 10 pixels in a partition of the usual size
        let partsize = set.partsize.min(ok.len()).max(1);
        let least = (10 * ok.len()).div_ceil(partsize);
        left_out = counts.iter().filter(|&(_, &c)| c < least).map(|(&k, &c)| (k, c)).collect();
        ok.retain(|&i| !left_out.iter().any(|&(k, _)| Some(Some(k)) == class_of(i)));
    }
    let n = ok.len();
    let partsize = set.partsize.min(n);
    let npart = n.checked_div(partsize).map_or(0, |k| k.min(set.max_partitions));
    if npart < 2 || partsize < 20 {
        return Err(format!(
            "{n} pixels with a trend: too few for two partitions of {} (lower the partition size, or test a larger \
             region)",
            set.partsize
        ));
    }

    // the spatial correlation, from a sample's residuals
    progress(0.4, "Fitting the spatial correlation");
    let sample: Vec<usize> =
        parts::sample_partitions(n, set.fit_n.min(n), 1, set.seed).iter().map(|&k| ok[k]).collect();
    let mut resids = vec![f64::NAN; sample.len() * nt];
    for (r, &i) in resids.chunks_mut(nt).zip(&sample) {
        if let Some(f) = ar_reml::fit(&px.values[i * nt..(i + 1) * nt], &design, q, set.gaps) {
            r.copy_from_slice(&f.residuals);
        }
    }
    let (sx, sy): (Vec<f64>, Vec<f64>) = sample.iter().map(|&i| (px.x[i], px.y[i])).unzip();
    let span = {
        let ext = |v: &[f64]| {
            v.iter().copied().fold(f64::NEG_INFINITY, f64::max) - v.iter().copied().fold(f64::INFINITY, f64::min)
        };
        ext(&sx).hypot(ext(&sy)).max(f64::MIN_POSITIVE)
    };
    let start = Correlation::Exponential { range: 0.1 * span };
    let cor = spatial::fit_correlation(&resids, nt, &sx, &sy, start, false, set.n_jobs)
        .ok_or("the spatial correlation of the residuals could not be fitted")?;
    let Correlation::Exponential { range } = cor.cor else { unreachable!() };
    stop()?;

    // the design: a mean, or a mean per class
    progress(0.5, "Testing the trends in partitions");
    let classes: Vec<i32> = match &px.classes {
        Some(_) => {
            let mut c: Vec<i32> = ok.iter().map(|&i| class_of(i).flatten().unwrap()).collect();
            c.sort_unstable();
            c.dedup();
            c
        }
        None => vec![],
    };
    let p = classes.len().max(1);
    let mut x = vec![0.0; n * p];
    for (k, &i) in ok.iter().enumerate() {
        let col = match class_of(i).flatten() {
            Some(c) => classes.binary_search(&c).unwrap(),
            None => 0,
        };
        x[col * n + k] = 1.0;
    }
    let ones = vec![1.0; n];
    let (xs, ys, resp): (Vec<f64>, Vec<f64>, Vec<f64>) =
        ok.iter().map(|&i| (px.x[i], px.y[i], slope[i])).fold((vec![], vec![], vec![]), |mut a, (x, y, s)| {
            a.0.push(x);
            a.1.push(y);
            a.2.push(s);
            a
        });
    let data = parts::Data { x: &xs, y: &ys, response: &resp, design: &x, p, design0: &ones, p0: 1 };
    let partitions = parts::sample_partitions(n, partsize, npart, set.seed);
    let opts = parts::Options {
        cor: cor.cor,
        scaled: false,
        nugget: set.nugget.map_or(Nugget::Ml, Nugget::Fixed),
        ncross: set.ncross,
        n_jobs: set.n_jobs,
    };
    let fit = parts::fit(&data, &partitions, partsize, &opts)
        .ok_or("a partition's covariance or design is singular (a class with too few pixels?)")?;
    progress(1.0, "Done");

    let means = (0..p)
        .map(|c| {
            let class = classes.get(c).copied();
            let members = ok.iter().filter(|&&i| class.is_none() || class_of(i).flatten() == class);
            let (mut pixels, mut up, mut down) = (0, 0, 0);
            for &i in members {
                pixels += 1;
                if pv[i] < set.alpha {
                    if slope[i] > 0.0 {
                        up += 1;
                    } else {
                        down += 1;
                    }
                }
            }
            Mean {
                class,
                pixels,
                estimate: fit.t_test.coefficients[c],
                se: fit.t_test.se[c],
                t: fit.t_test.t[c],
                p: fit.t_test.p[c],
                significant_up: up,
                significant_down: down,
            }
        })
        .collect();
    let nugget = fit.parts.iter().map(|f| f.nugget).sum::<f64>() / fit.parts.len() as f64;
    Ok(Outcome {
        means,
        left_out,
        p_chisqr: if p > 1 { fit.p_chisqr } else { None },
        f: fit.f,
        df1: fit.df1,
        df2: fit.df2,
        range,
        nugget,
        npart,
        partsize,
        fitted,
        tested: npart * partsize,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A grid of pixels in three vertical bands of classes, each with its trend, and smooth noise.
    fn region(nx: usize, ny: usize) -> Pixels {
        let nt = 15;
        let years: Vec<f64> = (0..nt).map(|t| 2005.5 + t as f64).collect();
        let mut seed = 12345u64;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f64 / (1u64 << 53) as f64 - 0.5
        };
        let field: Vec<f64> = (0..(nx + 6) * (ny + 6) * nt).map(|_| rnd()).collect();
        let (mut values, mut x, mut y, mut classes) = (vec![], vec![], vec![], vec![]);
        for r in 0..ny {
            for c in 0..nx {
                let class = (3 * c / nx) as i32;
                let trend = [0.0, 0.004, -0.003][class as usize];
                for t in 0..nt {
                    // the mean of a 5 x 5 window of independent noise: spatially correlated
                    let noise: f64 = (0..5)
                        .flat_map(|dr| (0..5).map(move |dc| (dr, dc)))
                        .map(|(dr, dc)| field[(t * (ny + 6) + r + dr) * (nx + 6) + c + dc])
                        .sum::<f64>()
                        / 5.0;
                    values.push(0.6 + trend * t as f64 + 0.02 * noise);
                }
                x.push(30.0 * c as f64);
                y.push(-30.0 * r as f64);
                classes.push(Some(class));
            }
        }
        Pixels { values, years, x, y, classes: Some(classes) }
    }

    #[test]
    fn classes_their_means_and_whether_they_differ() {
        let px = region(36, 30);
        let set = Settings { partsize: 200, nugget: Some(0.0), n_jobs: 2, ..Default::default() };
        let out = run(&px, &set, &|_, _| {}, &AtomicBool::new(false)).unwrap();
        assert_eq!(out.means.len(), 3);
        assert_eq!(out.npart, 5);
        assert!((out.means[1].estimate - 0.004).abs() < 1e-3, "{:?}", out.means[1]);
        assert!(out.means[1].p < 1e-6 && out.p_chisqr.unwrap() < 0.01);
        assert!(out.range > 0.0 && out.left_out.is_empty());
        // the region's mean alone
        let mean = Pixels { classes: None, ..px.clone() };
        let out = run(&mean, &set, &|_, _| {}, &AtomicBool::new(false)).unwrap();
        assert_eq!(out.means.len(), 1);
        assert!(out.p_chisqr.is_none());
    }

    #[test]
    fn small_classes_left_out_and_cancel() {
        let mut px = region(36, 30);
        // a class of 5 pixels
        for c in px.classes.as_mut().unwrap().iter_mut().take(5) {
            *c = Some(9);
        }
        let set = Settings { partsize: 200, nugget: Some(0.0), n_jobs: 1, ..Default::default() };
        let out = run(&px, &set, &|_, _| {}, &AtomicBool::new(false)).unwrap();
        assert_eq!(out.left_out, vec![(9, 5)]);
        assert_eq!(run(&px, &set, &|_, _| {}, &AtomicBool::new(true)), Err("cancelled".into()));
        let few = Settings { partsize: 2000, ..set };
        assert!(run(&px, &few, &|_, _| {}, &AtomicBool::new(false)).unwrap_err().contains("too few"));
    }
}
