//! Janus's field network against segmentation_models_pytorch's Unet(efficientnet-b3) with Trazo3's weights
//! (tests/data/fieldnet, written by tools/fieldnet_reference.py): the same images, the same scores to float32's
//! tolerance. The weights are found where fieldnet::weights looks (downloaded on first use, 204 MB); without them
//! and without network the test says so and passes.

use std::sync::atomic::AtomicBool;

use burn::prelude::*;
use janus_ml::fieldnet::{self, FieldNet};
use janus_ml::model::device;

fn read(name: &str) -> Vec<f32> {
    let path = format!("{}/tests/data/fieldnet/{name}", env!("CARGO_MANIFEST_DIR"));
    let b = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    b.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect()
}

#[test]
fn scores_are_smps() {
    let cache = std::env::temp_dir().join("janus-ml-weights");
    let ck = fieldnet::checkpoint("trazo3").unwrap();
    let weights = match fieldnet::weights(&cache, ck, &|_| {}, &AtomicBool::new(false)) {
        Ok(w) => w,
        Err(e) => {
            println!("skipped: no Trazo3 weights ({e})");
            return;
        }
    };
    let (dev, name) = device(std::env::var_os("JANUS_ML_GPU").is_some());
    let net = FieldNet::new(fieldnet::INPUTS, fieldnet::CLASSES, &dev).load_ftw(&weights).unwrap();
    let (b, h, w) = (2, 64, 96);
    let x = Tensor::<4>::from_data(TensorData::new(read("input.f32"), [b, 8, h, w]), &dev);
    let got: Vec<f32> = net.forward(x).into_data().try_to_vec().unwrap();
    let want = read("scores.f32");
    assert_eq!(got.len(), want.len());
    let worst = got.iter().zip(&want).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
    let scale = want.iter().fold(0f32, |m, v| m.max(v.abs()));
    // the class each pixel gets
    let arg = |v: &[f32], i: usize| {
        let plane = h * w;
        let (bi, p) = (i / plane, i % plane);
        (0..3).max_by(|&a, &c| v[(bi * 3 + a) * plane + p].total_cmp(&v[(bi * 3 + c) * plane + p])).unwrap()
    };
    let differ = (0..b * h * w).filter(|&i| arg(&got, i) != arg(&want, i)).count();
    println!(
        "field network on {name}: {} scores, the largest difference from smp's is {worst:.2e} (scores up to \
         {scale:.1}); {differ} of {} pixels change class",
        got.len(),
        b * h * w
    );
    assert!(worst < 1e-3 * scale.max(1.0), "the field network's scores differ from smp's by {worst}");
}
