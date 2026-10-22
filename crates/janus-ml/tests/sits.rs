//! Janus's TempCNN and L-TAE against sits' (tests/data/sits_*.json, written by tools/ml_sits_reference.R): sits'
//! own samples, the same training and test samples, sits' defaults; the test accuracy over the same seeds is
//! sits_tempcnn's and sits_lighttae's within a few points (not bit for bit: other frameworks, other random numbers).
//!
//!     cargo test -p janus-ml --release --test sits -- --ignored --nocapture      (JANUS_ML_GPU=1: on the GPU)

use std::sync::atomic::AtomicBool;

use janus_ml::ltae::LTaeHyper;
use janus_ml::model::{Net, device};
use janus_ml::tempcnn::TempCnnHyper;
use janus_ml::train::{TrainConfig, train};
use janus_ml::{Arch, Calendar, Norm, Series};
use serde_json::Value;

/// How far Janus's mean test accuracy may be from sits'.
const TOLERANCE: f64 = 0.03;

struct Set {
    train: (Series, Vec<u32>),
    test: (Series, Vec<u32>),
    calendar: Calendar,
    classes: usize,
}

fn load(name: &str) -> (Set, Value) {
    let path = format!("{}/tests/data/sits_{name}.json", env!("CARGO_MANIFEST_DIR"));
    let v: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))).unwrap();
    let labels: Vec<String> = v["labels"].as_array().unwrap().iter().map(|l| l.as_str().unwrap().to_string()).collect();
    // one band: R's length-1 vectors came unboxed (a string, rows of one number as numbers)
    let b = v["bands"].as_array().map_or(1, Vec::len);
    let timeline: Vec<String> =
        v["timeline"].as_array().unwrap().iter().map(|d| d.as_str().unwrap().to_string()).collect();
    let t = timeline.len();
    let day = |s: &str| {
        // days since 1970 of YYYY-MM-DD (Howard Hinnant's days_from_civil)
        let p: Vec<i64> = s.split('-').map(|x| x.parse().unwrap()).collect();
        let (y, m, d) = (if p[1] <= 2 { p[0] - 1 } else { p[0] }, p[1], p[2]);
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
        era * 146097 + (yoe * 365 + yoe / 4 - yoe / 100 + doy) - 719468
    };
    let days: Vec<i64> = timeline.iter().map(|s| day(s)).collect();
    let calendar = Calendar {
        labels: timeline.clone(),
        years: days.iter().map(|&d| 1970.0 + d as f64 / 365.2425).collect(),
        days: Some(days),
    };
    let mut parts = [(vec![], vec![]), (vec![], vec![])];
    for s in v["samples"].as_array().unwrap() {
        let k = (s["split"] == "test") as usize;
        let class = labels.iter().position(|l| l == s["label"].as_str().unwrap()).unwrap() as u32;
        // values: [t][b] rows of the time series
        for row in s["values"].as_array().unwrap() {
            match row.as_array() {
                Some(r) => parts[k].0.extend(r.iter().map(|x| x.as_f64().unwrap() as f32)),
                None => parts[k].0.push(row.as_f64().unwrap() as f32),
            }
        }
        parts[k].1.push(class);
    }
    let series = |(values, y): (Vec<f32>, Vec<u32>)| (Series { n: y.len(), t, b, values }, y);
    let [tr, te] = parts;
    (Set { train: series(tr), test: series(te), calendar, classes: labels.len() }, v)
}

fn accuracy(net: &Net, x: &Series, y: &[u32], dev: &burn::prelude::Device) -> f64 {
    use burn::prelude::*;
    let input = Tensor::<3>::from_data(TensorData::new(x.values.clone(), [x.n, x.t, x.b]), dev);
    let pred: Vec<i64> = net.forward(input).argmax(1).into_data().convert::<i64>().try_to_vec().unwrap();
    pred.iter().zip(y).filter(|(p, t)| **p == **t as i64).count() as f64 / y.len() as f64
}

fn compare(name: &str) {
    let (set, v) = load(name);
    let (dev, dev_name) = device(std::env::var_os("JANUS_ML_GPU").is_some());
    let seeds: Vec<u64> = v["seeds"].as_array().unwrap().iter().map(|s| s.as_u64().unwrap()).collect();
    let (mut xtr, ytr) = set.train.clone();
    let (mut xte, yte) = set.test.clone();
    let norm = Norm::fit(&xtr);
    norm.apply(&mut xtr);
    norm.apply(&mut xte);
    let mut failures = vec![];
    for (key, arch) in
        [("tempcnn", Arch::TempCnn(TempCnnHyper::default())), ("lighttae", Arch::LTae(LTaeHyper::default()))]
    {
        let sits: Vec<f64> = v["accuracy"][key].as_array().unwrap().iter().map(|a| a.as_f64().unwrap()).collect();
        let mut ours = vec![];
        for &seed in &seeds {
            let cfg = TrainConfig { seed, ..TrainConfig::for_arch(&arch) };
            let t0 = std::time::Instant::now();
            let tr =
                train(&xtr, &ytr, set.classes, &arch, &set.calendar, &cfg, &dev, &mut |_| {}, &AtomicBool::new(false))
                    .unwrap();
            let a = accuracy(&tr.net, &xte, &yte, &dev);
            println!(
                "{name} {key} seed {seed} on {dev_name}: {} epochs, test accuracy {a:.4} ({:.0} s)",
                tr.history.len(),
                t0.elapsed().as_secs_f64()
            );
            ours.push(a);
        }
        let mean = |a: &[f64]| a.iter().sum::<f64>() / a.len() as f64;
        let (m_ours, m_sits) = (mean(&ours), mean(&sits));
        println!("{name} {key}: Janus {m_ours:.4}, sits {m_sits:.4} ({:?})", sits);
        if m_ours < m_sits - TOLERANCE {
            failures.push(format!("{name} {key}: Janus {m_ours:.4} vs sits {m_sits:.4}"));
        }
    }
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
#[ignore = "trains 12 models: run with --ignored, in release"]
fn modis_ndvi_as_sits() {
    compare("samples_modis_ndvi");
}

#[test]
#[ignore = "trains 12 models: run with --ignored, in release"]
fn rondonia_as_sits() {
    compare("samples_l8_rondonia_2bands");
}
