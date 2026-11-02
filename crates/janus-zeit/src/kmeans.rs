//! k-means clustering of the pixels by their whole series (Janus' own tool, as scikit-learn's `KMeans`: k-means++
//! starts, Lloyd's iterations, the best of several starts; cluster.rs): fitted on pixels sampled over the run's
//! window as the SOM is (som.rs: the same sample, the same gap filling), then every pixel to its nearest centre.
//! The clusters ordered by their centres' mean value, low to high, so that the same settings give the same legend.

use serde_json::Value;

use crate::job::{Chunk, JobCtx};
use crate::pixel::Params;
use crate::rng::Pcg64;
use crate::som::{MAX_CLUSTERS, clusters_model, sampled_series};

/// The k-means fitted on a sample of the window (`sample`: [T][pixels] of rows spread over it): its centres as the
/// clusters' prototypes, their names and typical series.
pub fn fit(p: &Params, sample: &Chunk, ctx: &JobCtx) -> Result<Value, String> {
    let k = p.int("k");
    if !(2..=MAX_CLUSTERS).contains(&k) {
        return Err(format!("ValueError: from 2 to {MAX_CLUSTERS} clusters, got {k}"));
    }
    let k = k as usize;
    let t = sample.t;
    let n_sample = p.int("sample").max(1) as usize;
    let seed = p.int("seed").max(0) as u64;
    let x = sampled_series(p, sample, ctx);
    let n = x.len() / t.max(1);
    if n < k {
        return Err(format!("ValueError: only {n} sampled pixels have enough values for {k} clusters"));
    }
    let s_n = n_sample.min(n);
    let fitted: Vec<f64> = if n > s_n {
        let mut idx = Pcg64::new(seed).choice(n, s_n);
        idx.sort_unstable();
        idx.iter().flat_map(|&i| x[i as usize * t..(i as usize + 1) * t].iter().copied()).collect()
    } else {
        x.clone()
    };
    let starts = p.int("starts").max(1) as usize;
    let threads = if ctx.n_jobs > 0 { ctx.n_jobs as usize } else { 0 };
    let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().map_err(|e| e.to_string())?;
    let km = pool.install(|| crate::cluster::kmeans(&fitted, s_n, t, k, starts, seed));
    // low to high by the centres' mean value
    let mean = |c: usize| km.centres[c * t..(c + 1) * t].iter().sum::<f64>() / t.max(1) as f64;
    let mut order: Vec<usize> = (0..k).collect();
    order.sort_by(|&a, &b| mean(a).total_cmp(&mean(b)));
    let centres: Vec<f64> = order.iter().flat_map(|&c| km.centres[c * t..(c + 1) * t].iter().copied()).collect();
    Ok(clusters_model(&centres, k, &x, t, ctx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pixel::PixelInput;
    use serde_json::json;

    /// Three kinds of series (flat low, flat high, rising), some with a gap: three clusters, each one kind, in
    /// the order of their means.
    #[test]
    fn finds_the_kinds_of_series() {
        let t = 8;
        let years: Vec<f64> = (0..t).map(|i| 2000.0 + i as f64).collect();
        let n = 300;
        let mut stack = vec![0.0; t * n];
        for px in 0..n {
            for i in 0..t {
                let jitter = ((px * 7 + i * 3) % 11) as f64 * 0.002;
                let v = match px % 3 {
                    0 => 0.2,
                    1 => 0.8,
                    _ => 0.2 + 0.08 * i as f64,
                } + jitter;
                stack[i * n + px] = if px % 10 == 5 && i == 3 { f64::NAN } else { v };
            }
        }
        let chunk = Chunk { t, h: 1, w: n, stack, ..Default::default() };
        let given = [("k", json!(3)), ("sample", json!(1000))].into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        let p = Params::with_defaults("kmeans", &given).unwrap();
        let ctx = JobCtx { input: PixelInput { years, ..Default::default() }, n_jobs: 2, ..Default::default() };
        let model = fit(&p, &chunk, &ctx).unwrap();
        let protos: Vec<Vec<f64>> = serde_json::from_value(model["prototypes"].clone()).unwrap();
        assert_eq!(protos.len(), 3);
        assert!((protos[0][0] - 0.2).abs() < 0.02 && (protos[0][7] - 0.2).abs() < 0.02, "{protos:?}");
        assert!((protos[1][7] - 0.76).abs() < 0.02, "the rising one second: {protos:?}");
        assert!((protos[2][0] - 0.8).abs() < 0.02, "{protos:?}");
        let ctx = JobCtx { model: Some(model.clone()), ..ctx };
        let out = crate::som::chunk(&p, &chunk, &ctx).unwrap();
        for px in 0..n {
            let want = [1.0, 3.0, 2.0][px % 3];
            assert_eq!(out["cluster"][px], want, "pixel {px}");
        }
        let names = model["outputs"]["cluster"]["classes"].as_array().unwrap();
        assert_eq!(names[0], "Cluster 1 (33%)");
        // named by samples: two places of "low" in cluster 1, a place of two "rising" pixels in cluster 2 (one
        // place together), a place of "high" in cluster 3
        let mut named_model = model.clone();
        let picks = [0usize, 3, 2, 5, 1];
        let classes = ["low", "low", "rising", "rising", "high"];
        let places = [0usize, 1, 2, 2, 3];
        let mut s = vec![0.0; t * picks.len()];
        for (j, &px) in picks.iter().enumerate() {
            for i in 0..t {
                s[i * picks.len() + j] = chunk.stack[i * n + px];
            }
        }
        let samples = Chunk { t, h: 1, w: picks.len(), stack: s, ..Default::default() };
        let named: Vec<crate::som::Named> = (0..picks.len())
            .map(|j| crate::som::Named {
                class: classes[j],
                weight: 1.0 / places.iter().filter(|&&q| q == places[j]).count() as f64,
            })
            .collect();
        crate::som::name_by_samples(&mut named_model, &p, &samples, &named, &ctx);
        let names = named_model["outputs"]["cluster"]["classes"].as_array().unwrap();
        assert_eq!(names[0], "low (cluster 1, 33%)");
        assert_eq!(names[1], "rising (cluster 2, 33%)");
        assert_eq!(names[2], "high (cluster 3, 33%)");
        assert_eq!(named_model["outputs"]["cluster"]["class_samples"][1]["rising"], 2);
    }

    /// A cluster whose samples are split, none with two thirds, is "A or B"; one without samples keeps its number.
    #[test]
    fn mixed_and_empty_clusters() {
        let t = 2;
        let model0 = json!({"prototypes": [[0.0, 0.0], [1.0, 1.0]], "shares": [0.5, 0.5],
                            "outputs": {"cluster": {"classes": ["a", "b"]}}});
        let p = Params::with_defaults("kmeans", &Default::default()).unwrap();
        let ctx =
            JobCtx { input: PixelInput { years: vec![2000.0, 2001.0], ..Default::default() }, ..Default::default() };
        let samples = Chunk { t, h: 1, w: 3, stack: vec![0.0, 0.1, 0.0, 0.0, 0.1, 0.0], ..Default::default() };
        let named = [("forest", 1.0), ("pasture", 1.0), ("forest", 0.5)]
            .map(|(class, weight)| crate::som::Named { class, weight });
        let mut model = model0.clone();
        crate::som::name_by_samples(&mut model, &p, &samples, &named, &ctx);
        let names = model["outputs"]["cluster"]["classes"].as_array().unwrap();
        assert_eq!(names[0], "forest or pasture (cluster 1, 50%)");
        assert_eq!(names[1], "Cluster 2, 50%, no samples");
    }
}
