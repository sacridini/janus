//! The spatial folds against Zeit's `spatial_folds` (zeit/_folds.py, scipy, scikit-learn): the same blocks dealt in
//! rows and as a checkerboard, the same buffers, Ward's same clusters, k-means as tight as scikit-learn's, and
//! kNNDM's folds as near the map's distances (its random draws are not numpy's). tests/data/folds_zeit.json is
//! Zeit's output for the points below (the script in the comment at the end).

use janus_ml::folds::{self, Clustering, Selection, Shape};

fn pts(n: usize) -> Vec<[f64; 2]> {
    (0..n)
        .map(|i| {
            [(i * 37 % 101) as f64 * 13.7 + (i * i % 17) as f64, (i * 53 % 97) as f64 * 11.3 + (i % 7) as f64 * 3.0]
        })
        .collect()
}

/// 50 clumps of 20 places, and a map around them.
fn clumps() -> (Vec<f64>, Vec<f64>) {
    let big: Vec<f64> = (0..1000)
        .flat_map(|i: usize| {
            let c = i / 20;
            [
                (c * 37 % 50) as f64 * 60.0 + (i * 7 % 13) as f64 * 2.0,
                (c * 23 % 50) as f64 * 55.0 + (i * 11 % 17) as f64 * 1.5,
            ]
        })
        .collect();
    let g: Vec<f64> = (0..32).map(|i| i as f64 * 120.0 - 500.0).collect();
    let pred: Vec<f64> = g.iter().flat_map(|&a| g.iter().map(move |&b| [a, b])).take(1000).flatten().collect();
    (big, pred)
}

fn zeit() -> serde_json::Value {
    serde_json::from_str(include_str!("data/folds_zeit.json")).unwrap()
}

fn ints(v: &serde_json::Value) -> Vec<i64> {
    v.as_array().unwrap().iter().map(|x| x.as_i64().unwrap()).collect()
}

/// Two labellings the same partition.
fn same_partition(a: &[i64], b: &[i64]) -> bool {
    let mut map = std::collections::HashMap::new();
    a.iter().zip(b).all(|(x, y)| *map.entry(x).or_insert(y) == y)
        && map.values().collect::<std::collections::HashSet<_>>().len() == map.len()
}

#[test]
fn blocks_and_buffers_as_zeit() {
    let z = zeit();
    let xy = pts(300);
    for (key, shape) in [("sys_hexagon", Shape::Hexagon), ("sys_square", Shape::Square)] {
        let f = folds::blocks(&xy, 5, 90.0, shape, Selection::Systematic, None, 1, 0).unwrap();
        assert_eq!(f.fold, ints(&z[key]), "{key}");
    }
    let f = folds::blocks(&xy, 2, 90.0, Shape::Square, Selection::Checkerboard, None, 1, 0).unwrap();
    assert_eq!(f.fold, ints(&z["checker"]));
    let b = folds::buffer(&xy, 60.0).unwrap();
    let counts: Vec<i64> = b.exclude.iter().map(|e| e.len() as i64).collect();
    assert_eq!(counts, ints(&z["buffer"]));
}

#[test]
fn clusters_as_scipy_and_scikit_learn() {
    let z = zeit();
    let xy: Vec<f64> = pts(300).into_iter().flatten().collect();
    let tree = janus_ml::cluster::ward(&xy, 300, 2);
    let ours: Vec<i64> = janus_ml::cluster::cut(&tree, 300, 7).iter().map(|&l| l as i64).collect();
    assert!(same_partition(&ours, &ints(&z["ward7"])), "Ward's clusters differ from scipy's");
    let km = janus_ml::cluster::kmeans(&xy, 300, 2, 6, 10, 0);
    let theirs = z["kmeans_inertia"].as_f64().unwrap();
    assert!(km.inertia <= theirs * 1.01, "inertia {} against scikit-learn's {theirs}", km.inertia);
}

#[test]
fn knndm_as_near_the_map_as_zeit() {
    let z = zeit();
    let (big, pred) = clumps();
    let w_random = z["w_random"].as_f64().unwrap();
    for (key, clustering) in [("knndm_hierarchical", Clustering::Hierarchical), ("knndm_kmeans", Clustering::Kmeans)] {
        let t = std::time::Instant::now();
        let f = folds::knndm(&big, 1000, 2, &pred, 1000, 5, clustering, 0.5, 100, 0, 4).unwrap();
        let theirs = &z[key];
        let w = theirs["w"].as_f64().unwrap();
        println!(
            "{key}: W {:.1} (q {}) in {:.3} s; Zeit's W {w:.1} (q {}) in {:.3} s; a random 5-fold's {w_random:.1}",
            f.w,
            f.q,
            t.elapsed().as_secs_f64(),
            theirs["q"],
            theirs["seconds"].as_f64().unwrap()
        );
        assert!(f.w < 0.6 * w_random && f.w < 1.15 * w, "{key}: W {} against Zeit's {w}", f.w);
    }
}

// The fixture, from Zeit (its conda environment, OMP_NUM_THREADS=4):
//
//   from zeit import _folds as F; from scipy.cluster.hierarchy import linkage, fcluster
//   xy = pts(300)                         # as above
//   F._blocks(P, xy, 5, 90.0, "hexagon" | "square", "systematic", None, 1, rng)[0]
//   F._blocks(P, xy, 2, 90.0, "square", "checkerboard", None, 1, rng)[0]
//   [len(r) - 1 for r in cKDTree(xy).query_ball_point(xy, r=60.0)]
//   fcluster(linkage(xy, method="ward"), t=7, criterion="maxclust") - 1
//   KMeans(n_clusters=6, n_init=10, random_state=0).fit(xy).inertia_
//   F._knndm(big, pred, 5, "hierarchical" | "kmeans", 0.5, 100, default_rng(0), 0, 4)   # big, pred: clumps()
