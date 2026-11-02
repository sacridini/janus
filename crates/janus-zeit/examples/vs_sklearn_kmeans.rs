//! The k-means tool's fit (janus_zeit::cluster::kmeans, Lloyd's from k-means++ starts) against scikit-learn's
//! KMeans (tools/bench_kmeans_vs_sklearn.py runs it): the points of `X.f64` (n x p, row-major) into k clusters, the
//! best of `STARTS` starts, on `THREADS` threads, the best time of `RUNS`; prints the seconds, the inertia, the
//! iterations of the start kept, and writes each point's cluster to `X.labels` (u8).
//!
//!     cargo run --release -p janus-zeit --example vs_sklearn_kmeans -- X.f64 N P K STARTS SEED THREADS RUNS

use std::time::Instant;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let num = |i: usize| -> usize { a[i].parse().expect("a number") };
    let (path, n, p, k, starts, seed, threads, runs) = (&a[0], num(1), num(2), num(3), num(4), num(5), num(6), num(7));
    let bytes = std::fs::read(path).expect("the points");
    let x: Vec<f64> = bytes.as_chunks::<8>().0.iter().map(|b| f64::from_le_bytes(*b)).collect();
    assert_eq!(x.len(), n * p, "n x p values");
    let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap();
    let mut best = f64::INFINITY;
    let mut km = None;
    for _ in 0..runs.max(1) {
        let t = Instant::now();
        let r = pool.install(|| janus_zeit::cluster::kmeans(&x, n, p, k, starts, seed as u64));
        best = best.min(t.elapsed().as_secs_f64());
        km = Some(r);
    }
    let km = km.unwrap();
    let labels: Vec<u8> = km.labels.iter().map(|&l| l as u8).collect();
    std::fs::write(format!("{}.labels", path.trim_end_matches(".f64")), labels).unwrap();
    println!("{{\"seconds\": {best}, \"inertia\": {}, \"iterations\": {}}}", km.inertia, km.iterations);
}
