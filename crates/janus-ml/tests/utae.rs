//! Janus's U-TAE against zeit.ai.utae's (a port of the authors' code) with the authors' PASTIS weights
//! (tests/data/utae, written by tools/utae_reference.py): the same patches (one with dates without observation),
//! the same scores to float32's tolerance. The weights are found where utae::pastis_weights looks (taken from the
//! authors' zip on first use); without them and without network the test says so and passes.

use std::sync::atomic::AtomicBool;

use burn::prelude::*;
use janus_ml::model::device;
use janus_ml::utae::{self, Utae, UtaeHyper};

fn read(name: &str) -> Vec<f32> {
    let path = format!("{}/tests/data/utae/{name}", env!("CARGO_MANIFEST_DIR"));
    let b = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    b.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect()
}

#[test]
fn scores_are_zeits() {
    let cache = std::env::temp_dir().join("janus-ml-weights");
    let weights = match utae::pastis_weights(&cache, &|_| {}, &AtomicBool::new(false)) {
        Ok(w) => w,
        Err(e) => {
            println!("skipped: no U-TAE weights ({e})");
            return;
        }
    };
    let (dev, name) = device(std::env::var_os("JANUS_ML_GPU").is_some());
    let net = Utae::new(&UtaeHyper::default(), 10, utae::PASTIS_CLASSES, &dev).load_authors(&weights).unwrap().valid();
    let (b, t, c, h, w) = (2, 6, 10, 24, 24);
    let x = Tensor::<5>::from_data(TensorData::new(read("input.f32"), [b, t, c, h, w]), &dev);
    let pos = Tensor::<2>::from_data(TensorData::new(read("positions.f32"), [b, t]), &dev);
    let got: Vec<f32> = net.forward(x, pos).into_data().try_to_vec().unwrap();
    let want = read("scores.f32");
    assert_eq!(got.len(), want.len());
    let worst = got.iter().zip(&want).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
    let scale = want.iter().fold(0f32, |m, v| m.max(v.abs()));
    println!(
        "U-TAE on {name}: {} scores, the largest difference from zeit's is {worst:.2e} (scores up to {scale:.1})",
        got.len()
    );
    assert!(worst < 1e-3 * scale.max(1.0), "U-TAE's scores differ from zeit's by {worst}");
}
