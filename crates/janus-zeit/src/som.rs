//! SOM clustering (zeit_bridge/tool_som.py): a Self-Organizing Map trained on pixels sampled over the run's window
//! (zeit.som: MiniSom's initialisation and online training, zeit-core's engine), then every pixel to its nearest
//! prototype, the cluster's typical series.

use serde_json::{Value, json};
use zeit_core::som::{Grid, Schedule, predict_bmus, train_online};

use crate::job::{Chunk, ChunkOut, JobCtx};
use crate::pixel::Params;
use crate::rng::{Mt19937, Pcg64};
use crate::upstream::np_sum;

const MAX_CLUSTERS: i64 = 12;

/// Each pixel's series ([pixels][T]) with every value finite: gaps filled linearly in time when enough dates
/// have one, else NaN (the bridge's `fill_gaps`).
pub(crate) fn fill_gaps(v: &[f64], t: usize, x: &[f64], p: &Params) -> Vec<f64> {
    let n = v.len() / t.max(1);
    let mut out = vec![f64::NAN; v.len()];
    let skip = p.str("gaps") == "skip";
    let need = ((p.f64("min_valid") / 100.0 * t as f64).ceil() as usize).max(2);
    for px in 0..n {
        let s = &v[px * t..(px + 1) * t];
        let o = &mut out[px * t..(px + 1) * t];
        let ok: Vec<bool> = s.iter().map(|x| x.is_finite()).collect();
        let count = ok.iter().filter(|&&k| k).count();
        if skip {
            if count == t {
                o.copy_from_slice(s);
            }
            continue;
        }
        if count < need {
            continue;
        }
        // the dates with a value before and after each date
        let mut prev = vec![-1i64; t];
        let mut last = -1;
        for i in 0..t {
            if ok[i] {
                last = i as i64;
            }
            prev[i] = last;
        }
        let mut next = vec![t as i64; t];
        let mut first = t as i64;
        for i in (0..t).rev() {
            if ok[i] {
                first = i as i64;
            }
            next[i] = first;
        }
        for i in 0..t {
            let clip = |k: i64| k.clamp(0, t as i64 - 1) as usize;
            let a_i = clip(if prev[i] >= 0 { prev[i] } else { next[i] });
            let b_i = clip(if next[i] < t as i64 { next[i] } else { prev[i] });
            let (a, b, xa, xb) = (s[a_i], s[b_i], x[a_i], x[b_i]);
            let f = if xb > xa { (x[i] - xa) / (xb - xa) } else { 0.0 };
            o[i] = a + (b - a) * f;
        }
    }
    out
}

/// The SOM trained on a sample of the window (`sample`: [T][pixels] of rows spread over it): its prototypes
/// (the neurons a sampled pixel chose) and the clusters' names and typical series.
pub fn fit(p: &Params, sample: &Chunk, ctx: &JobCtx) -> Result<Value, String> {
    let (gx, gy) = (p.int("x"), p.int("y"));
    let k_n = (gx * gy) as usize;
    if gx * gy > MAX_CLUSTERS {
        return Err(format!("ValueError: at most {MAX_CLUSTERS} clusters (columns x rows), got {gx} x {gy}"));
    }
    let t = sample.t;
    let flat = sample.by_pixel(&sample.stack);
    let mut cand: Vec<usize> =
        (0..sample.pixels()).filter(|&i| flat[i * t..(i + 1) * t].iter().any(|v| v.is_finite())).collect();
    let n_sample = p.int("sample").max(1) as usize;
    let seed = p.int("seed").max(0) as u64;
    if cand.len() > 3 * n_sample {
        // enough to keep the sample after the gaps; bounded memory
        let mut idx = Pcg64::new(seed).choice(cand.len(), 3 * n_sample);
        idx.sort_unstable();
        cand = idx.into_iter().map(|i| cand[i as usize]).collect();
    }
    let picked: Vec<f64> = cand.iter().flat_map(|&i| flat[i * t..(i + 1) * t].iter().copied()).collect();
    let filled = fill_gaps(&picked, t, &ctx.input.years, p);
    let x: Vec<f64> = (0..cand.len())
        .filter(|&i| filled[i * t..(i + 1) * t].iter().all(|v| v.is_finite()))
        .flat_map(|i| filled[i * t..(i + 1) * t].iter().copied())
        .collect();
    let n = x.len() / t.max(1);
    if n < k_n {
        return Err(format!("ValueError: only {n} sampled pixels have enough values for {k_n} clusters"));
    }
    let s_n = n_sample.min(n);
    // zeit.som: its own sample of the pixels (a fresh generator of the seed), MiniSom's initial draws and its
    // online training
    let samples: Vec<f64> = if n > s_n {
        let mut idx = Pcg64::new(seed).choice(n, s_n);
        idx.sort_unstable();
        idx.iter().flat_map(|&i| x[i as usize * t..(i as usize + 1) * t].iter().copied()).collect()
    } else {
        x.clone()
    };
    let mut mt = Mt19937::new(seed as u32);
    for _ in 0..k_n * t {
        mt.next_double(); // the random weights MiniSom draws first, then replaces
    }
    let mut weights: Vec<f64> = (0..k_n)
        .flat_map(|_| {
            let k = mt.randint(s_n as u64) as usize;
            samples[k * t..(k + 1) * t].to_vec()
        })
        .collect();
    let num_iters = p.int("passes").max(1) as usize * s_n;
    let order: Vec<usize> = (0..num_iters).map(|i| i % s_n).collect();
    let (gx, gy) = (gx as usize, gy as usize);
    let xx: Vec<f64> = (0..k_n).map(|k| (k / gy) as f64).collect();
    let yy: Vec<f64> = (0..k_n).map(|k| (k % gy) as f64).collect();
    let grid = Grid::new(gx, gy, &xx, &yy).map_err(|e| e.to_string())?;
    let schedule = Schedule { learning_rate: 0.5, sigma: 1.0, lr_decay: 2, sigma_decay: 0, neighborhood: 0 };
    train_online(&mut weights, &samples, t, &order, num_iters, false, &schedule, &grid);
    // the pixels of each neuron (every pixel with values); the neurons none chose left out
    let bmus = predict_bmus(&x, t, &weights, k_n, ctx.n_jobs);
    let mut counts = vec![0usize; k_n];
    for b in bmus {
        counts[b as usize] += 1;
    }
    let used: Vec<usize> = (0..k_n).filter(|&k| counts[k] > 0).collect();
    let total = used.iter().map(|&k| counts[k]).sum::<usize>().max(1) as f64;
    let protos: Vec<Vec<f64>> = used.iter().map(|&k| weights[k * t..(k + 1) * t].to_vec()).collect();
    let names: Vec<String> = used
        .iter()
        .enumerate()
        .map(|(i, &k)| format!("Cluster {} ({:.0}%)", i + 1, 100.0 * counts[k] as f64 / total))
        .collect();
    let series: Vec<Value> = protos.iter().map(|y| json!({"x": ctx.input.years, "y": y})).collect();
    Ok(json!({"prototypes": protos, "outputs": {"cluster": {"classes": names, "class_series": series}}}))
}

pub fn chunk(p: &Params, chunk: &Chunk, ctx: &JobCtx) -> Result<ChunkOut, String> {
    let protos: Vec<Vec<f64>> = ctx
        .model
        .as_ref()
        .and_then(|m| serde_json::from_value(m["prototypes"].clone()).ok())
        .ok_or("the SOM was not trained")?;
    let t = chunk.t;
    let x = fill_gaps(&chunk.by_pixel(&chunk.stack), t, &ctx.input.years, p);
    let ww: Vec<f64> = protos.iter().map(|w| np_sum(&w.iter().map(|v| v * v).collect::<Vec<_>>())).collect();
    let n = chunk.pixels();
    let (mut cluster, mut dist) = (vec![f64::NAN; n], vec![f64::NAN; n]);
    for px in 0..n {
        let s = &x[px * t..(px + 1) * t];
        if !s.iter().all(|v| v.is_finite()) {
            continue;
        }
        let xx = np_sum(&s.iter().map(|v| v * v).collect::<Vec<_>>());
        let mut best = (0, f64::INFINITY);
        for (k, w) in protos.iter().enumerate() {
            let dot: f64 = s.iter().zip(w).map(|(a, b)| a * b).sum();
            let d2 = xx - 2.0 * dot + ww[k];
            if d2 < best.1 {
                best = (k, d2);
            }
        }
        cluster[px] = (best.0 + 1) as f64;
        dist[px] = best.1.max(0.0).sqrt();
    }
    let mut res = ChunkOut::new();
    res.insert("cluster".into(), cluster);
    res.insert("distance".into(), dist);
    Ok(res)
}
