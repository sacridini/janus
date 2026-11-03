//! The benchmark of tools/bench_validation_vs_survey.py: the areas of the transitions between two maps of classes
//! inside the strata (`mapped_transitions`, the maps read on the strata's grid) and their estimates from a stratified
//! sample by the direct, difference and ratio estimators (`assess`), timed, as JSON.
//!
//!     vs_survey STRATA FROM TO POINTS.csv THREADS OUT.json
//!
//! POINTS.csv: `row,col,ref_from,ref_to` (the reference class codes at the two dates), a header first. The classes
//! of the transitions are "a -> b" of the codes.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use janus_validation::{
    ARROW, AreaEstimator, AssessInput, MapKind, MapRule, Progress, Strata, Stratum, assess, mapped_transitions,
    read_grid, row_areas,
};
use serde_json::{Value, json};

fn read_all(path: &str) -> (usize, Vec<f64>) {
    let ds = gdal::Dataset::open(path).expect("a raster");
    let (w, h) = ds.raster_size();
    let b = ds.rasterband(1).unwrap().read_as::<f64>((0, 0), (w, h), (w, h), None).unwrap();
    (w, b.into_shape_and_vec().1)
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (strata_path, from, to, points, threads, out) = (&a[1], &a[2], &a[3], &a[4], a[5].parse().unwrap(), &a[6]);
    let grid = read_grid(strata_path).unwrap();
    let (row_area, _) = row_areas(&grid);
    // the strata's areas and the points' strata, pixels and values (outside the timings: what a project holds)
    let (w, s) = read_all(strata_path);
    // (pixels counted by row, then times the row's area: as the strata of a project)
    let mut pixels: BTreeMap<(i32, usize), i64> = BTreeMap::new();
    for (i, &v) in s.iter().enumerate() {
        if v >= 1.0 {
            *pixels.entry((v as i32, i / w)).or_insert(0) += 1;
        }
    }
    let mut area: BTreeMap<i32, f64> = BTreeMap::new();
    for (&(h, y), &n) in &pixels {
        *area.entry(h).or_insert(0.0) += n as f64 * row_area[y];
    }
    let (_, mf) = read_all(from);
    let (_, mt) = read_all(to);
    let text = std::fs::read_to_string(points).unwrap();
    let pts: Vec<[usize; 4]> = text
        .lines()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let v: Vec<usize> = l.split(',').map(|x| x.trim().parse().unwrap()).collect();
            [v[0], v[1], v[2], v[3]]
        })
        .collect();
    let at = |r: &[f64], p: &[usize; 4]| r[p[0] * w + p[1]] as i32;
    let strata = Strata {
        grid: grid.clone(),
        strata: area
            .iter()
            .map(|(&code, &area)| Stratum { code, name: code.to_string(), area, ..Default::default() })
            .collect(),
        row_area: row_area.clone(),
        ..Default::default()
    };
    let rule = |path: &str| MapRule { path: path.into(), band: 1, kind: MapKind::Classes, ..Default::default() };
    // the mapped transitions: the best of three
    let mut prog = Progress::default();
    prog.threads = threads;
    let mut count_s = f64::INFINITY;
    let mut mapped = BTreeMap::new();
    for _ in 0..3 {
        let t = Instant::now();
        mapped = mapped_transitions(&rule(from), &rule(to), strata_path, &strata, Some(&prog)).unwrap();
        count_s = count_s.min(t.elapsed().as_secs_f64());
    }
    // the classes: the maps' transitions and the reference's
    let name = |a: i32, b: i32| format!("{a}{ARROW}{b}");
    let mut pairs: BTreeSet<(i32, i32)> = mapped.keys().copied().collect();
    pairs.extend(pts.iter().map(|p| (p[2] as i32, p[3] as i32)));
    let pairs: Vec<(i32, i32)> = pairs.into_iter().collect();
    let idx = |a: i32, b: i32| pairs.iter().position(|&q| q == (a, b));
    let order: Vec<i32> = area.keys().copied().collect();
    let mut inp = AssessInput {
        stratum_area: area.values().copied().collect(),
        stratum: pts.iter().map(|p| order.iter().position(|&o| o == at(&s, p))).collect(),
        map: pts.iter().map(|p| idx(at(&mf, p), at(&mt, p))).collect(),
        reference: pts.iter().map(|p| idx(p[2] as i32, p[3] as i32)).collect(),
        classes: pairs.iter().map(|&(a, b)| name(a, b)).collect(),
        mapped: pairs.iter().map(|k| mapped.get(k).copied().unwrap_or(0.0)).collect(),
        confidence: 0.95,
        estimator: AreaEstimator::Direct,
    };
    let num = |v: f64| if v.is_finite() { json!(v) } else { Value::Null };
    let mut res = serde_json::Map::new();
    for e in AreaEstimator::ALL {
        inp.estimator = e;
        // the estimates of every transition: the median of many runs
        let reps = 200;
        let mut ts: Vec<f64> = (0..reps)
            .map(|_| {
                let t = Instant::now();
                std::hint::black_box(assess(std::hint::black_box(&inp)));
                t.elapsed().as_secs_f64()
            })
            .collect();
        ts.sort_by(f64::total_cmp);
        let r = assess(&inp);
        let classes: serde_json::Map<String, Value> = r
            .classes
            .iter()
            .enumerate()
            .map(|(c, n)| {
                let (ar, u, p) = (&r.area[c], &r.users[c], &r.producers[c]);
                (
                    n.clone(),
                    json!({"mapped": num(ar.mapped), "area": [num(ar.est), num(ar.se)], "users": [num(u.est), num(u.se)],
                           "producers": [num(p.est), num(p.se)]}),
                )
            })
            .collect();
        res.insert(
            e.name().into(),
            json!({"assess_s": ts[reps / 2], "n": r.n, "overall": [num(r.overall.est), num(r.overall.se)],
                   "classes": classes}),
        );
    }
    let mapped_json: serde_json::Map<String, Value> =
        mapped.iter().map(|(&(a, b), &v)| (name(a, b), json!(v))).collect();
    let j = json!({"count_s": count_s, "threads": threads, "mapped": mapped_json, "estimators": res});
    std::fs::write(out, serde_json::to_string_pretty(&j).unwrap()).unwrap();
}
