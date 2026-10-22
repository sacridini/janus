//! U-TAE on a made-up series of fields (rectangles of three classes of trajectories, with clouds and dates
//! without observation): patches with labels of the image's upper half only, a U-TAE trained on them (on the
//! CPU, or the GPU with JANUS_ML_GPU=1), kept in a file and read back, and the whole image classified in
//! overlapping windows: the lower half, never seen, mostly right.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use janus_cube::{BandSelection, open_cube};
use janus_ml::apply::{ApplySpec, apply};
use janus_ml::dense::{NONE, Standard, read_patches, train_utae, utae_config};
use janus_ml::model::{Header, Model, device};
use janus_ml::train::{Rng, TrainConfig};
use janus_ml::utae::UtaeHyper;
use janus_ml::{Arch, Calendar, Class, InputBand, Norm};

const W: usize = 64;
const H: usize = 64;
const T: usize = 8;

/// The fields: a grid of 16 x 16 rectangles, each of a class.
fn class_at(x: usize, y: usize) -> u8 {
    (((x / 16) * 7 + (y / 16) * 3) % 3) as u8
}

#[test]
fn utae_learns_fields() {
    let dir = std::env::temp_dir().join("janus-ml-test-utae");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let drv = gdal::DriverManager::get_driver_by_name("GTiff").unwrap();
    let mut rng = Rng::new(9);
    for t in 0..T {
        let mut ds =
            drv.create_with_band_type::<f32, _>(dir.join(format!("s_2021-{:02}-01.tif", t + 2)), W, H, 2).unwrap();
        ds.set_geo_transform(&[500000.0, 10.0, 0.0, 7500000.0, 0.0, -10.0]).unwrap();
        let absent = t == 3; // a date without any observation
        for b in 0..2 {
            let data: Vec<f32> = (0..W * H)
                .map(|p| {
                    let c = class_at(p % W, p / W);
                    let s = (t as f32 / T as f32 * std::f32::consts::TAU).sin();
                    let v = match c {
                        0 => 0.7,
                        1 => 0.4 + 0.3 * s,
                        _ => 0.2 + 0.05 * t as f32,
                    } + (rng.next_u64() % 100) as f32 / 1000.0
                        - 0.05;
                    if absent || rng.next_u64().is_multiple_of(10) { -1.0 } else { v * (1.0 + b as f32) }
                })
                .collect();
            let mut band = ds.rasterband(b + 1).unwrap();
            band.set_no_data_value(Some(-1.0)).unwrap();
            band.write((0, 0), (W, H), &mut gdal::raster::Buffer::new((W, H), data)).unwrap();
        }
    }
    let info = Arc::new(open_cube(&[dir.to_string_lossy().into_owned()], &BandSelection::default()).unwrap());
    let inputs = vec![InputBand::source(&info, 1), InputBand::source(&info, 2)];
    // labels of the upper half
    let labels: Vec<u8> = (0..W * H).map(|p| if p / W < H / 2 { class_at(p % W, p / W) } else { NONE }).collect();
    let mut patches = read_patches(&info, &inputs, &labels, 32, &|_| {}, &AtomicBool::new(false)).unwrap();
    let standard = Standard::fit(&patches);
    let plane = 32 * 32;
    standard.apply(&mut patches.x, patches.c, plane);
    // more patches: the same ones shifted (a small set otherwise)
    println!("{} patches of 32 px", patches.n);
    let hyper = UtaeHyper { patch: 32, overlap: 8, ..Default::default() };
    let (dev, name) = device(std::env::var_os("JANUS_ML_GPU").is_some());
    let calendar = Calendar::of(&info);
    let cfg = TrainConfig { epochs: 60, patience: 60, validation_split: 0.25, ..utae_config() };
    let t0 = std::time::Instant::now();
    let trained = train_utae(
        &patches,
        3,
        &hyper,
        &calendar.day_offsets(),
        &cfg,
        None,
        &dev,
        &mut |_| {},
        &AtomicBool::new(false),
    )
    .unwrap();
    println!(
        "U-TAE on {name}: {} epochs in {:.1} s, last training accuracy {:.3}",
        trained.history.len(),
        t0.elapsed().as_secs_f64(),
        trained.history.last().unwrap().train_acc
    );
    let header = Header {
        format: 1,
        janus: "test".into(),
        arch: Arch::Utae(hyper),
        inputs: inputs.clone(),
        calendar,
        norm: Norm::default(),
        standard: Some(standard),
        classes: ["a", "b", "c"].iter().map(|n| Class { name: n.to_string(), color: [0, 0, 0] }).collect(),
        min_valid: 0.0,
        series: String::new(),
        training: serde_json::Value::Null,
        created: String::new(),
    };
    let model = Model { header, net: trained.net, device: dev.clone() };
    let path = dir.join("utae.janusmodel").to_string_lossy().into_owned();
    model.save(&path).unwrap();
    let back = Model::load(&path, &dev).unwrap();
    let out = dir.join("out");
    let spec = ApplySpec {
        window: [0, 0, W, H],
        output_dir: out.to_string_lossy().into_owned(),
        stem: "utae".into(),
        batch: 0,
    };
    let res = apply(&spec, &info, &back, &|_, _| {}, &AtomicBool::new(false)).unwrap();
    let ds = gdal::Dataset::open(out.join("utae_class.tif")).unwrap();
    let cls = ds.rasterband(1).unwrap().read_as::<u8>((0, 0), (W, H), (W, H), None).unwrap();
    let (mut right, mut lower) = (0, 0);
    for (p, &c) in cls.data().iter().enumerate() {
        if p / W >= H / 2 {
            lower += 1;
            right += (c == class_at(p % W, p / W) + 1) as usize;
        }
    }
    let acc = right as f64 / lower as f64;
    println!("U-TAE: {} pixels classified; the unseen lower half {acc:.3} right", res.classified);
    assert_eq!(res.classified, W * H);
    assert!(acc > 0.8, "the unseen half is {acc} right");
}
