//! The Random Forest against scikit-learn's and R's randomForest (tools/bench_forest.py runs it): the samples of
//! `xtr.f32` (n x p) and their classes `ytr.u8`, the test samples `xte.f32` and `yte.u8`. For each seed, grows the
//! forest (the best of a few runs on 1 thread and on 4) and classifies the test samples; prints a JSON line a run
//! (times in ms, the test accuracy, the out-of-bag accuracy).
//!
//!     cargo run --release -p janus-ml --example forest_bench -- DIR P TREES SEEDS

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use janus_ml::Series;
use janus_ml::forest::{ForestHyper, train};

fn read_f32(path: &Path) -> Vec<f32> {
    std::fs::read(path).unwrap().as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect()
}

fn best_ms(runs: usize, mut f: impl FnMut()) -> f64 {
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
    let (p, trees, seeds): (usize, usize, u64) = (a[2].parse().unwrap(), a[3].parse().unwrap(), a[4].parse().unwrap());
    let xtr = read_f32(&dir.join("xtr.f32"));
    let ytr: Vec<u32> = std::fs::read(dir.join("ytr.u8")).unwrap().iter().map(|&c| u32::from(c)).collect();
    let xte = read_f32(&dir.join("xte.f32"));
    let yte = std::fs::read(dir.join("yte.u8")).unwrap();
    let k = (*ytr.iter().max().unwrap() as usize + 1).max(usize::from(*yte.iter().max().unwrap()) + 1);
    let n = ytr.len();
    let x = Series { n, t: 1, b: p, values: xtr };
    let all: Vec<usize> = (0..n).collect();
    let h = ForestHyper { trees, ..Default::default() };
    let runs = if n * trees > 5_000_000 { 1 } else { 3 };
    for threads in [1, 4] {
        let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap();
        for seed in 0..seeds {
            let mut f = None;
            let t_fit = pool.install(|| {
                best_ms(runs, || {
                    f = Some(train(&x, &ytr, k, &h, &all, &[], seed, &mut |_| {}, &AtomicBool::new(false)).unwrap())
                })
            });
            let f = f.unwrap();
            let mut probs = vec![];
            let t_pred = pool.install(|| best_ms(runs, || probs = f.forest.predict(&xte, yte.len())));
            let right = probs
                .chunks(k)
                .zip(&yte)
                .filter(|(q, c)| (0..k).fold(0, |b, j| if q[j] > q[b] { j } else { b }) == usize::from(**c))
                .count();
            let oob = f.history.last().map_or(f64::NAN, |e| e.train_acc);
            println!(
                "{{\"threads\": {threads}, \"seed\": {seed}, \"fit_ms\": {t_fit:.1}, \"predict_ms\": {t_pred:.1}, \
                 \"accuracy\": {:.6}, \"oob\": {oob:.6}}}",
                right as f64 / yte.len() as f64
            );
        }
    }
}
