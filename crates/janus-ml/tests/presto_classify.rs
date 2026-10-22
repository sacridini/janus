//! Presto on a made-up Sentinel-2 year (monthly, six named bands, clouds) in UTM: its inputs found by the bands'
//! names, a classifier trained on its encodings of points, kept in a file and applied to the image; and the
//! encodings written as an embedding layer. Needs the authors' weights (see presto::weights): without them and
//! without network the test says so and passes.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use janus_cube::{BandSelection, open_cube};
use janus_ml::apply::{ApplySpec, apply};
use janus_ml::data::read_points;
use janus_ml::model::{Header, Model, device};
use janus_ml::presto::{self, PrestoHyper, PrestoInputs};
use janus_ml::train::{Rng, TrainConfig, train};
use janus_ml::{Arch, Calendar, Class, Norm};

const W: usize = 36;
const H: usize = 24;
const NAMES: [&str; 6] = ["blue", "green", "red", "nir", "swir1", "swir2"];

fn class_at(x: usize) -> u32 {
    (x * 3 / W) as u32
}

/// Reflectance (0-1) of a class's band in a month.
fn value(class: u32, band: usize, month: usize, rng: &mut Rng) -> f32 {
    let noise = (rng.next_u64() % 1000) as f32 / 1000.0 * 0.02 - 0.01;
    let s = (month as f32 / 12.0 * std::f32::consts::TAU).sin();
    let veg = match class {
        0 => 0.8,             // forest
        1 => 0.45 + 0.35 * s, // crop
        _ => 0.15,            // bare
    };
    let base = [0.05, 0.08, 0.1 - 0.06 * veg, 0.15 + 0.35 * veg, 0.3 - 0.1 * veg, 0.25 - 0.12 * veg][band];
    (base + noise).max(0.001)
}

#[test]
fn presto_classifies_and_embeds() {
    let cache = std::env::temp_dir().join("janus-ml-weights");
    let weights = match presto::weights(&cache, &|_| {}, &AtomicBool::new(false)) {
        Ok(w) => w,
        Err(e) => {
            println!("skipped: no Presto weights ({e})");
            return;
        }
    };
    let dir = std::env::temp_dir().join("janus-ml-test-presto");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let drv = gdal::DriverManager::get_driver_by_name("GTiff").unwrap();
    let utm = gdal::spatial_ref::SpatialRef::from_epsg(32722).unwrap().to_wkt().unwrap();
    let mut rng = Rng::new(11);
    for m in 0..12 {
        let mut ds =
            drv.create_with_band_type::<f32, _>(dir.join(format!("s2_2021-{:02}-15.tif", m + 1)), W, H, 6).unwrap();
        ds.set_geo_transform(&[500000.0, 10.0, 0.0, 7500000.0, 0.0, -10.0]).unwrap();
        ds.set_projection(&utm).unwrap();
        let cloudy: Vec<bool> = (0..W * H).map(|_| rng.next_u64().is_multiple_of(8)).collect();
        for (b, name) in NAMES.iter().enumerate() {
            let data: Vec<f32> =
                (0..W * H).map(|p| if cloudy[p] { -1.0 } else { value(class_at(p % W), b, m, &mut rng) }).collect();
            let mut band = ds.rasterband(b + 1).unwrap();
            band.set_no_data_value(Some(-1.0)).unwrap();
            band.set_description(name).unwrap();
            band.write((0, 0), (W, H), &mut gdal::raster::Buffer::new((W, H), data)).unwrap();
        }
    }
    let info = Arc::new(open_cube(&[dir.to_string_lossy().into_owned()], &BandSelection::default()).unwrap());
    assert_eq!((info.t(), info.bands_per_date), (12, 6));
    let ranges = vec![(0.0, 0.6); 6];
    let inputs = PrestoInputs::of(&info, &ranges, Some(2021)).unwrap();
    let read = inputs.read_bands();
    assert_eq!(read.len(), 6, "every band is one of Presto's inputs");
    assert_eq!(inputs.reflectance_scale, 1e4);

    // samples at points
    let mut rng = Rng::new(5);
    let points: Vec<(usize, usize)> =
        (0..150).map(|_| ((rng.next_u64() % W as u64) as usize, (rng.next_u64() % H as u64) as usize)).collect();
    let raw = read_points(&info, &read, &points);
    let pixels: Vec<(f64, f64)> = points.iter().map(|&(x, y)| (x as f64, y as f64)).collect();
    let (x, keep) = presto::classifier_input(&info, &inputs, &raw, &pixels).unwrap();
    let idx: Vec<usize> = (0..x.n).filter(|&i| keep[i]).collect();
    let x = x.select(&idx);
    let y: Vec<u32> = idx.iter().map(|&i| class_at(points[i].0)).collect();
    let (dev, name) = device(std::env::var_os("JANUS_ML_GPU").is_some());
    let arch = Arch::Presto(PrestoHyper { inputs: inputs.clone(), hidden: vec![], weights: Some(weights.clone()) });
    let calendar = Calendar::of(&info);
    let cfg = TrainConfig::for_arch(&arch);
    let t0 = std::time::Instant::now();
    let trained = train(&x, &y, 3, &arch, &calendar, &cfg, &dev, &mut |_| {}, &AtomicBool::new(false)).unwrap();
    let (oa, _, _) = trained.accuracies();
    println!(
        "Presto head on {name}: {} epochs in {:.1} s, validation accuracy {oa:.3}",
        trained.history.len(),
        t0.elapsed().as_secs_f64()
    );
    assert!(oa > 0.85, "validation accuracy {oa}");

    let header = Header {
        format: 1,
        janus: "test".into(),
        arch,
        inputs: read.clone(),
        calendar,
        norm: Norm::default(),
        classes: ["forest", "crop", "bare"].iter().map(|n| Class { name: n.to_string(), color: [1, 2, 3] }).collect(),
        min_valid: 0.0,
        series: String::new(),
        training: serde_json::Value::Null,
        created: String::new(),
    };
    let model = Model { header, net: trained.net, device: dev.clone() };
    let path = dir.join("presto.janusmodel").to_string_lossy().into_owned();
    model.save(&path).unwrap();
    let back = Model::load(&path, &dev).unwrap();
    let out = dir.join("out");
    let spec = ApplySpec {
        window: [0, 0, W, H],
        output_dir: out.to_string_lossy().into_owned(),
        stem: "presto".into(),
        batch: 200,
    };
    apply(&spec, &info, &back, &|_, _| {}, &AtomicBool::new(false)).unwrap();
    let ds = gdal::Dataset::open(out.join("presto_class.tif")).unwrap();
    let cls = ds.rasterband(1).unwrap().read_as::<u8>((0, 0), (W, H), (W, H), None).unwrap();
    let right = cls.data().iter().enumerate().filter(|(p, c)| **c as u32 == class_at(p % W) + 1).count();
    let acc = right as f64 / (W * H) as f64;
    println!("Presto class map: {acc:.3} right");
    assert!(acc > 0.85, "the class map is {acc} right");

    // the encodings as an embedding layer
    let emb = dir.join("presto_2021.tif");
    let n =
        presto::embed_window(&info, &inputs, [0, 0, W, H], &weights, &emb, &dev, &|_, _| {}, &AtomicBool::new(false))
            .unwrap();
    let e = gdal::Dataset::open(&emb).unwrap();
    use gdal::Metadata;
    assert_eq!(e.raster_count(), 128);
    assert_eq!(n, W * H);
    assert!(e.metadata_item("ZEIT_EMBEDDING", "").is_some_and(|t| t.contains("presto")));
    println!("Presto embeddings: {n} pixels, 128 bands, tagged");
}
