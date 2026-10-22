//! The whole path on a made-up series: three classes of trajectories (stable, seasonal, a rise) in three strips of
//! a 2-band cube with clouds; samples read at points, gaps filled, scaled; TempCNN and L-TAE trained (on the CPU,
//! or the GPU with JANUS_ML_GPU=1), kept in a file and read back (the same probabilities), applied to the image.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use janus_cube::{BandSelection, open_cube};
use janus_ml::apply::{ApplySpec, apply};
use janus_ml::data::{fill_gaps, read_points};
use janus_ml::ltae::LTaeHyper;
use janus_ml::model::{Header, Model, device};
use janus_ml::tempcnn::TempCnnHyper;
use janus_ml::train::{Rng, TrainConfig, train};
use janus_ml::{Arch, Calendar, Class, InputBand, Norm};

const W: usize = 48;
const H: usize = 30;
const T: usize = 24;

fn class_at(x: usize) -> u32 {
    (x * 3 / W) as u32
}

fn value(class: u32, t: usize, rng: &mut Rng) -> f32 {
    let noise = (rng.next_u64() % 1000) as f32 / 1000.0 * 0.08 - 0.04;
    let s = (t as f32 / 12.0 * std::f32::consts::TAU).sin();
    noise
        + match class {
            0 => 0.8,
            1 => 0.5 + 0.3 * s,
            _ => 0.2 + 0.6 * t as f32 / T as f32,
        }
}

fn make_cube(dir: &std::path::Path) {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).unwrap();
    let drv = gdal::DriverManager::get_driver_by_name("GTiff").unwrap();
    let mut rng = Rng::new(7);
    for t in 0..T {
        let (y, m) = (2020 + t / 12, t % 12 + 1);
        let path = dir.join(format!("s2_{y}-{m:02}-15.tif"));
        let mut ds = drv.create_with_band_type::<f32, _>(&path, W, H, 2).unwrap();
        ds.set_geo_transform(&[500000.0, 30.0, 0.0, 7500000.0, 0.0, -30.0]).unwrap();
        let (mut a, mut b) = (vec![0f32; W * H], vec![0f32; W * H]);
        for p in 0..W * H {
            let v = value(class_at(p % W), t, &mut rng);
            let cloud = rng.next_u64().is_multiple_of(10);
            a[p] = if cloud { -9999.0 } else { v };
            b[p] = if cloud { -9999.0 } else { 0.5 * v + 0.1 };
        }
        for (k, data) in [a, b].into_iter().enumerate() {
            let mut band = ds.rasterband(k + 1).unwrap();
            band.set_no_data_value(Some(-9999.0)).unwrap();
            band.write((0, 0), (W, H), &mut gdal::raster::Buffer::new((W, H), data)).unwrap();
        }
    }
}

fn run(arch: Arch, epochs: usize, tag: &str) {
    let dir = std::env::temp_dir().join(format!("janus-ml-test-{tag}"));
    make_cube(&dir);
    let info = Arc::new(open_cube(&[dir.to_string_lossy().into_owned()], &BandSelection::default()).unwrap());
    assert_eq!(info.t(), T);
    assert_eq!(info.bands_per_date, 2);
    let inputs = vec![InputBand::source(&info, 1), InputBand::source(&info, 2)];
    let calendar = Calendar::of(&info);
    let mut rng = Rng::new(3);
    let points: Vec<(usize, usize)> =
        (0..240).map(|_| ((rng.next_u64() % W as u64) as usize, (rng.next_u64() % H as u64) as usize)).collect();
    let mut s = read_points(&info, &inputs, &points);
    let keep = fill_gaps(&mut s, &calendar.years, 50.0);
    let idx: Vec<usize> = (0..s.n).filter(|&i| keep[i]).collect();
    let mut s = s.select(&idx);
    let y: Vec<u32> = idx.iter().map(|&i| class_at(points[i].0)).collect();
    let norm = Norm::fit(&s);
    norm.apply(&mut s);
    let gpu = std::env::var_os("JANUS_ML_GPU").is_some();
    let (dev, name) = device(gpu);
    let batch: usize = std::env::var("JANUS_ML_BATCH")
        .ok()
        .and_then(|b| b.parse().ok())
        .unwrap_or(TrainConfig::for_arch(&arch).batch_size);
    let cfg = TrainConfig { epochs, batch_size: batch, ..TrainConfig::for_arch(&arch) };
    let t0 = std::time::Instant::now();
    let verbose = std::env::var_os("JANUS_ML_VERBOSE").is_some();
    let mut show = |e: &janus_ml::Epoch| {
        if verbose {
            println!(
                "  {} train {:.4} {:.3} val {:.4} {:.3}",
                e.epoch, e.train_loss, e.train_acc, e.val_loss, e.val_acc
            )
        }
    };
    let trained = train(&s, &y, 3, &arch, &calendar, &cfg, &dev, &mut show, &AtomicBool::new(false)).unwrap();
    let (oa, _, _) = trained.accuracies();
    println!(
        "{tag} on {name}: {} epochs in {:.1} s, best {}, validation accuracy {oa:.3} ({} samples)",
        trained.history.len(),
        t0.elapsed().as_secs_f64(),
        trained.best_epoch,
        trained.n_val
    );
    assert!(oa > 0.9, "{tag}: validation accuracy {oa}");

    // the file and back: the same probabilities
    let header = Header {
        format: 1,
        janus: "test".into(),
        arch: arch.clone(),
        inputs: inputs.clone(),
        calendar: calendar.clone(),
        norm,
        classes: ["stable", "seasonal", "rise"]
            .iter()
            .map(|n| Class { name: n.to_string(), color: [10, 20, 30] })
            .collect(),
        min_valid: 50.0,
        series: info.description.clone(),
        training: trained.summary(&cfg, &[0, 0, 0]),
        created: String::new(),
    };
    let model = Model { header, net: trained.net, device: dev.clone() };
    let path = dir.join(format!("{tag}.janusmodel")).to_string_lossy().into_owned();
    model.save(&path).unwrap();
    let back = Model::load(&path, &dev).unwrap();
    assert_eq!(back.header, model.header);
    let (a, b) = (model.predict(&s.values, s.n, 64), back.predict(&s.values, s.n, 100));
    let most = a.iter().zip(&b).map(|(p, q)| (p - q).abs()).fold(0f32, f32::max);
    assert!(most < 1e-5, "{tag}: probabilities differ by {most} after the file");

    // applied to the image
    let out = dir.join("out");
    let spec = ApplySpec {
        window: [0, 0, W, H],
        output_dir: out.to_string_lossy().into_owned(),
        stem: tag.into(),
        batch: 300,
    };
    let res = apply(&spec, &info, &back, &|_, _| {}, &AtomicBool::new(false)).unwrap();
    let ds = gdal::Dataset::open(out.join(format!("{tag}_class.tif"))).unwrap();
    let cls = ds.rasterband(1).unwrap().read_as::<u8>((0, 0), (W, H), (W, H), None).unwrap();
    let right = cls.data().iter().enumerate().filter(|(p, c)| **c as u32 == class_at(p % W) + 1).count();
    let acc = right as f64 / (W * H) as f64;
    println!("{tag}: {} of {} pixels classified, {:.3} right", res.classified, W * H, acc);
    assert!(acc > 0.9, "{tag}: the class map is {acc} right");
    assert_eq!(res.outputs.len(), 3);
}

#[test]
fn tempcnn_learns_and_classifies() {
    run(Arch::TempCnn(TempCnnHyper::default()), 30, "tempcnn");
}

#[test]
fn ltae_learns_and_classifies() {
    run(Arch::LTae(LTaeHyper::default()), 40, "ltae");
}
