//! The few-shot classes and the direction of change against scikit-learn (tools/bench_embeddings_sklearn.py runs
//! it): a store of two years read from `t0.f32` and `t1.f32` (pixels x d, values on the store's grid of
//! `range` / 127), the samples of `samples.f32` (n x d) and their classes `labels.u8`. Times the fit (with its held-out
//! accuracy), the classes of year 1 and the direction basis of the pair (1, 0), the best of a few runs on 1 thread and
//! on all but one, and writes the weights, the classes and scores, and the basis for the comparison.
//!
//!     cargo run --release -p janus-embed --example vs_sklearn -- DIR W H D RANGE LAMBDA THRESHOLD

use std::path::Path;
use std::time::Instant;

use janus_embed::classify::{Method, classify_year, fit};
use janus_embed::direction::fit_direction;
use janus_embed::store::EmbeddingStore;

fn read_f32(path: &Path) -> Vec<f32> {
    std::fs::read(path).unwrap().as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect()
}

fn write_f32(path: &Path, v: &[f32]) {
    std::fs::write(path, v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>()).unwrap();
}

fn best_ms(runs: usize, mut f: impl FnMut()) -> f64 {
    f();
    (0..runs)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed().as_secs_f64() * 1000.0
        })
        .fold(f64::INFINITY, f64::min)
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let dir = Path::new(&a[1]);
    let (w, h, d): (usize, usize, usize) = (a[2].parse().unwrap(), a[3].parse().unwrap(), a[4].parse().unwrap());
    let (range, lambda, threshold): (f32, f64, f32) =
        (a[5].parse().unwrap(), a[6].parse().unwrap(), a[7].parse().unwrap());
    let years = [read_f32(&dir.join("t0.f32")), read_f32(&dir.join("t1.f32"))];
    let s = EmbeddingStore::synthetic(w, h, 2, d, range, |t, p, v| v.copy_from_slice(&years[t][p * d..(p + 1) * d]));
    let x = read_f32(&dir.join("samples.f32"));
    let y = std::fs::read(dir.join("labels.u8")).unwrap();
    let k = usize::from(*y.iter().max().unwrap()) + 1;
    let mut classes = vec![vec![]; k];
    for (i, &c) in y.iter().enumerate() {
        classes[usize::from(c)].push(x[i * d..(i + 1) * d].to_vec());
    }
    let all = std::thread::available_parallelism().map_or(2, |n| n.get()).saturating_sub(1).max(1);
    let mut times = vec![];
    for threads in [1, all] {
        let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap();
        let mut c = None;
        let t_fit = best_ms(5, || c = Some(fit(&classes, Method::Logistic, lambda, None).unwrap()));
        let c = c.unwrap();
        let mut out = (vec![], vec![]);
        let t_apply = best_ms(3, || out = classify_year(&s, 1, &c, 0.0, &pool));
        let mut basis = None;
        let t_dir = best_ms(3, || basis = Some(fit_direction(&s, &[(1, 0)], threshold, 2.0, &pool)));
        let basis = basis.unwrap();
        if threads == 1 {
            write_f32(&dir.join("janus_w.f32"), &c.w);
            write_f32(&dir.join("janus_b.f32"), &c.b);
            std::fs::write(dir.join("janus_class.u8"), &out.0).unwrap();
            write_f32(&dir.join("janus_score.f32"), &out.1);
            write_f32(&dir.join("janus_comps.f32"), &basis.comps);
            write_f32(&dir.join("janus_mean.f32"), &basis.mean);
        }
        times.push(format!(
            "{{\"threads\": {threads}, \"fit_ms\": {t_fit}, \"apply_ms\": {t_apply}, \"direction_ms\": {t_dir}, \
             \"cv_acc\": {}, \"cv_folds\": {}, \"iterations\": {}, \"direction_samples\": {}, \"eig\": {:?}}}",
            c.cv_acc.unwrap_or(f64::NAN),
            c.cv_folds,
            c.iterations,
            basis.samples,
            basis.eig
        ));
    }
    println!("[{}]", times.join(", "));
}
