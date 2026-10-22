//! Janus's Presto against zeit.ai.presto's (tests/data/presto.json, written by tools/presto_reference.py): the same
//! raw series (Sentinel-2 reflectance 0-1, Sentinel-1 dB, SRTM, with gaps), the authors' weights, the same
//! encodings to float32's tolerance. The weights are found where presto::weights looks (JANUS_WEIGHTS, Zeit's
//! cache, else downloaded); without them and without network the test says so and passes.

use std::sync::atomic::AtomicBool;

use janus_ml::model::device;
use janus_ml::presto::{self, Encoder, PrestoInputs};
use janus_ml::{InputBand, Series};
use serde_json::Value;

#[test]
fn encodings_are_zeits() {
    let path = format!("{}/tests/data/presto.json", env!("CARGO_MANIFEST_DIR"));
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let names: Vec<String> = v["bands"].as_array().unwrap().iter().map(|b| b.as_str().unwrap().to_string()).collect();
    let cache = std::env::temp_dir().join("janus-ml-weights");
    let weights = match presto::weights(&cache, &|_| {}, &AtomicBool::new(false)) {
        Ok(w) => w,
        Err(e) => {
            println!("skipped: no Presto weights ({e})");
            return;
        }
    };
    let (dev, dev_name) = device(std::env::var_os("JANUS_ML_GPU").is_some());
    let enc = Encoder::pretrained(&weights, &dev).unwrap();

    // the inputs by name, the raw values of the bands read in Presto's order
    let found = presto::match_names(&names, &|_| None);
    let bands: Vec<Option<InputBand>> = found
        .iter()
        .map(|k| k.map(|k| InputBand { kind: "band".into(), band: k as i32 + 1, name: names[k].clone() }))
        .collect();
    let inputs = PrestoInputs { bands, reflectance_scale: 1e4, s1_linear: false, year: None };
    let read: Vec<usize> = found.iter().flatten().copied().collect();
    let px = v["values"].as_array().unwrap();
    let (n, t) = (px.len(), px[0].as_array().unwrap().len());
    let mut values = Vec::with_capacity(n * t * read.len());
    for p in px {
        for row in p.as_array().unwrap() {
            let row = row.as_array().unwrap();
            values.extend(read.iter().map(|&k| row[k].as_f64().map_or(f32::NAN, |x| x as f32)));
        }
    }
    let raw = Series { n, t, b: read.len(), values };
    let steps: Vec<Vec<usize>> = (0..t).map(|d| vec![d]).collect();
    let x = presto::prepare(&raw, &inputs, &steps);
    let months: Vec<i64> = v["months"].as_array().unwrap().iter().map(|m| m.as_i64().unwrap() - 1).collect();
    let latlon: Vec<f32> = v["latlon"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|p| p.as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32))
        .collect();

    let idx: Vec<usize> = (0..n).filter(|&i| presto::observed(&x, i)).collect();
    let sel = x.select(&idx);
    let ll: Vec<f32> = idx.iter().flat_map(|&i| [latlon[2 * i], latlon[2 * i + 1]]).collect();
    let got = presto::encode(&enc, &sel, &months, &ll, 64, &dev);
    let want = v["embeddings"].as_array().unwrap();
    let expected_encoded = want.iter().filter(|e| e[0].is_number()).count();
    assert_eq!(idx.len(), expected_encoded, "the pixels encoded");
    let mut worst = 0f32;
    for (j, &i) in idx.iter().enumerate() {
        for d in 0..presto::DIMS {
            let w = want[i][d].as_f64().unwrap() as f32;
            worst = worst.max((got[j * presto::DIMS + d] - w).abs());
        }
    }
    println!("{} encodings on {dev_name}: the largest difference from zeit's is {worst:.2e}", idx.len());
    assert!(worst < 2e-4, "Presto's encodings differ from zeit's by {worst}");
}
