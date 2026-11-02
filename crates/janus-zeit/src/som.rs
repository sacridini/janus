//! SOM clustering (zeit_bridge/tool_som.py): a Self-Organizing Map trained on pixels sampled over the run's window
//! (zeit.som: MiniSom's initialisation and online training, zeit-core's engine), then every pixel to its nearest
//! prototype, the cluster's typical series.

use serde_json::{Value, json};
use zeit_core::som::{Grid, Schedule, predict_bmus, train_online};

use crate::job::{Chunk, ChunkOut, JobCtx};
use crate::pixel::Params;
use crate::rng::{Mt19937, Pcg64};
use crate::upstream::np_sum;

/// The most clusters a map shows (its Paired colours).
pub(crate) const MAX_CLUSTERS: i64 = 12;

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

/// The pixels of a sample of the window (`sample`: [T][pixels] of rows spread over it) a model is fitted on: up to
/// 3 x the "sample" parameter of those with a value (drawn with the seed), their gaps filled (`fill_gaps`), those
/// still with a gap left out; [n][T].
pub(crate) fn sampled_series(p: &Params, sample: &Chunk, ctx: &JobCtx) -> Vec<f64> {
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
    (0..cand.len())
        .filter(|&i| filled[i * t..(i + 1) * t].iter().all(|v| v.is_finite()))
        .flat_map(|i| filled[i * t..(i + 1) * t].iter().copied())
        .collect()
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
    let n_sample = p.int("sample").max(1) as usize;
    let seed = p.int("seed").max(0) as u64;
    let x = sampled_series(p, sample, ctx);
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
    Ok(clusters_model(&weights, k_n, &x, t, ctx))
}

/// A clustering's model from its `k_n` prototypes ([k][T]) and the sampled pixels `x` ([n][T]): the prototypes
/// some pixel chose (the others left out), each cluster's share of the pixels, name and typical series.
pub(crate) fn clusters_model(weights: &[f64], k_n: usize, x: &[f64], t: usize, ctx: &JobCtx) -> Value {
    let bmus = predict_bmus(x, t, weights, k_n, ctx.n_jobs);
    let mut counts = vec![0usize; k_n];
    for b in bmus {
        counts[b as usize] += 1;
    }
    let used: Vec<usize> = (0..k_n).filter(|&k| counts[k] > 0).collect();
    let total = used.iter().map(|&k| counts[k]).sum::<usize>().max(1) as f64;
    let protos: Vec<Vec<f64>> = used.iter().map(|&k| weights[k * t..(k + 1) * t].to_vec()).collect();
    let shares: Vec<f64> = used.iter().map(|&k| counts[k] as f64 / total).collect();
    let names: Vec<String> =
        shares.iter().enumerate().map(|(i, s)| format!("Cluster {} ({:.0}%)", i + 1, 100.0 * s)).collect();
    let series: Vec<Value> = protos.iter().map(|y| json!({"x": ctx.input.years, "y": y})).collect();
    json!({
        "prototypes": protos,
        "shares": shares,
        "outputs": {"cluster": {"classes": names, "class_series": series}},
    })
}

/// Each pixel's nearest prototype (0-based) and its distance, None without a value on every date (`x`: [n][T],
/// its gaps filled).
fn nearest(x: &[f64], t: usize, protos: &[Vec<f64>]) -> Vec<Option<(usize, f64)>> {
    let ww: Vec<f64> = protos.iter().map(|w| np_sum(&w.iter().map(|v| v * v).collect::<Vec<_>>())).collect();
    x.chunks(t.max(1))
        .map(|s| {
            if !s.iter().all(|v| v.is_finite()) {
                return None;
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
            Some((best.0, best.1.max(0.0).sqrt()))
        })
        .collect()
}

fn prototypes(model: Option<&Value>) -> Result<Vec<Vec<f64>>, String> {
    model
        .and_then(|m| serde_json::from_value(m["prototypes"].clone()).ok())
        .ok_or_else(|| "the clusters were not fitted".to_string())
}

/// A sample (a pixel of a place whose class is known) naming the clusters: its class and its place's weight (1
/// over the place's pixels read, so that every place counts as one).
pub struct Named<'a> {
    pub class: &'a str,
    pub weight: f64,
}

/// The clusters named by the samples in them (`chunk`: their pixels, 1 x N, as the run reads them; `named` the
/// class and weight of each): a cluster takes the class of most of its samples (weighed by place), "A or B" when
/// none has two thirds of them, its number alone when no sample is in it. The samples in each cluster, by class,
/// as the output's "class_samples".
pub fn name_by_samples(model: &mut Value, p: &Params, chunk: &Chunk, named: &[Named], ctx: &JobCtx) {
    let Ok(protos) = prototypes(Some(model)) else { return };
    let t = chunk.t;
    let x = fill_gaps(&chunk.by_pixel(&chunk.stack), t, &ctx.input.years, p);
    let classes: Vec<&str> = {
        let mut c: Vec<&str> = named.iter().map(|s| s.class).collect();
        c.sort_unstable();
        c.dedup();
        c
    };
    let mut votes = vec![vec![0.0f64; classes.len()]; protos.len()];
    let mut pixels = vec![vec![0usize; classes.len()]; protos.len()];
    for (s, near) in named.iter().zip(nearest(&x, t, &protos)) {
        if let Some((k, _)) = near {
            let c = classes.binary_search(&s.class).unwrap_or(0);
            votes[k][c] += s.weight;
            pixels[k][c] += 1;
        }
    }
    let shares: Vec<f64> = serde_json::from_value(model["shares"].clone()).unwrap_or_default();
    let mut names = vec![];
    let mut held = vec![];
    for (k, v) in votes.iter().enumerate() {
        let share = shares.get(k).map_or(String::new(), |s| format!(", {:.0}%", 100.0 * s));
        let total: f64 = v.iter().sum();
        let mut by: Vec<usize> = (0..classes.len()).filter(|&c| v[c] > 0.0).collect();
        by.sort_by(|&a, &b| v[b].total_cmp(&v[a]));
        names.push(match by.as_slice() {
            [] => format!("Cluster {}{share}, no samples", k + 1),
            [a, b, ..] if 3.0 * v[*a] < 2.0 * total => {
                format!("{} or {} (cluster {}{share})", classes[*a], classes[*b], k + 1)
            }
            [a, ..] => format!("{} (cluster {}{share})", classes[*a], k + 1),
        });
        held.push(Value::Object(
            by.iter().map(|&c| (classes[c].to_string(), json!(pixels[k][c]))).collect::<serde_json::Map<_, _>>(),
        ));
    }
    let out = &mut model["outputs"]["cluster"];
    out["classes"] = json!(names);
    out["class_samples"] = json!(held);
}

/// Every pixel of the chunk to its nearest prototype (the SOM's or the k-means' centres): its cluster (1-based)
/// and the distance to it.
pub fn chunk(p: &Params, chunk: &Chunk, ctx: &JobCtx) -> Result<ChunkOut, String> {
    let protos = prototypes(ctx.model.as_ref())?;
    let t = chunk.t;
    let x = fill_gaps(&chunk.by_pixel(&chunk.stack), t, &ctx.input.years, p);
    let n = chunk.pixels();
    let (mut cluster, mut dist) = (vec![f64::NAN; n], vec![f64::NAN; n]);
    for (px, near) in nearest(&x, t, &protos).into_iter().enumerate() {
        if let Some((k, d)) = near {
            cluster[px] = (k + 1) as f64;
            dist[px] = d;
        }
    }
    let mut res = ChunkOut::new();
    res.insert("cluster".into(), cluster);
    res.insert("distance".into(), dist);
    Ok(res)
}
