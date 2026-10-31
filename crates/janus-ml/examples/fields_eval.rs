//! The field network on labelled chips (WRI Trazo's or Fields of The World's layout: s2_images/window_a and
//! window_b, label_masks/semantic_2class and instance), measured as ftw-tools' `ftw model test` measures it and on
//! the fields themselves:
//!
//! - pixels: the interior's IoU, precision and recall against semantic_3class's interior (`ftw model test -p3
//!   -t3`; FIELDS_TRUTH=2: semantic_2class's fields, boundaries included);
//! - objects as ftw-tools counts them: the 4-connected sets of interior pixels of the prediction and of the mask,
//!   a true one matched at IoU > 0.5;
//! - parcels: each labelled field (the instance mask) against the parcels janus_ml::fields finds (the interiors'
//!   sets, ftw-tools' polygonize, or the watershed that gives the fields their boundaries), matched at IoU > 0.5.
//!
//!     cargo run --release --example fields_eval -- CHECKPOINT|MODEL.janusfields DIR [DIR ...]
//!
//! (JANUS_ML_GPU=1 for the GPU; FIELDS_SEEDS=50 the watershed's seed probability (or distance) in percent;
//! FIELDS_SERIES the chips' monthly series for a temporal model, as fields_train reads them.)

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use burn::prelude::*;
use gdal::Dataset;
use janus_ml::fieldnet::{self, FieldNet};
use janus_ml::fields::{self, Parcels, Probabilities};
use janus_ml::fields_train::{FieldArch, FieldModel, FieldNetwork};
use janus_ml::model::device;

fn read_bands(path: &Path) -> (Vec<f32>, usize, usize, usize) {
    let ds = Dataset::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let (w, h) = ds.raster_size();
    let n = ds.raster_count();
    let mut v = Vec::with_capacity(n * w * h);
    for b in 1..=n {
        let band = ds.rasterband(b).unwrap();
        v.extend(band.read_as::<f32>((0, 0), (w, h), (w, h), None).unwrap().data());
    }
    (v, n, w, h)
}

#[derive(Default, Clone, Copy)]
struct Counts {
    tp: u64,
    fp: u64,
    fn_: u64,
}

impl Counts {
    fn add(&mut self, o: Counts) {
        (self.tp, self.fp, self.fn_) = (self.tp + o.tp, self.fp + o.fp, self.fn_ + o.fn_);
    }
    fn precision(&self) -> f64 {
        self.tp as f64 / (self.tp + self.fp).max(1) as f64
    }
    fn recall(&self) -> f64 {
        self.tp as f64 / (self.tp + self.fn_).max(1) as f64
    }
    fn iou(&self) -> f64 {
        self.tp as f64 / (self.tp + self.fp + self.fn_).max(1) as f64
    }
}

/// True labelled objects matched by predicted ones at IoU > 0.5 (each pair's overlap counted pixel by pixel).
fn match_objects(truth: &[u32], pred: &[u32]) -> Counts {
    let mut area_t: HashMap<u32, u64> = HashMap::new();
    let mut area_p: HashMap<u32, u64> = HashMap::new();
    let mut inter: HashMap<(u32, u32), u64> = HashMap::new();
    for (&t, &p) in truth.iter().zip(pred) {
        if t > 0 {
            *area_t.entry(t).or_default() += 1;
        }
        if p > 0 {
            *area_p.entry(p).or_default() += 1;
        }
        if t > 0 && p > 0 {
            *inter.entry((t, p)).or_default() += 1;
        }
    }
    let mut matched_t = std::collections::HashSet::new();
    let mut matched_p = std::collections::HashSet::new();
    for (&(t, p), &i) in &inter {
        let u = area_t[&t] + area_p[&p] - i;
        if i as f64 / u as f64 > 0.5 {
            matched_t.insert(t);
            matched_p.insert(p);
        }
    }
    Counts {
        tp: matched_t.len() as u64,
        fp: (area_p.len() - matched_p.len()) as u64,
        fn_: (area_t.len() - matched_t.len()) as u64,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: fields_eval CHECKPOINT DIR [DIR ...]");
        std::process::exit(2);
    }
    let seeds: u8 = std::env::var("FIELDS_SEEDS").ok().and_then(|s| s.parse().ok()).unwrap_or(50);
    let (dev, name) = device(std::env::var_os("JANUS_ML_GPU").is_some());
    let model = if args[0].ends_with(".janusfields") {
        FieldModel::load(Path::new(&args[0]), &dev).unwrap()
    } else {
        let net = FieldNet::new(fieldnet::INPUTS, fieldnet::CLASSES, &dev).load_ftw(Path::new(&args[0])).unwrap();
        FieldModel {
            header: janus_ml::fields_train::FieldHeader {
                arch: FieldArch::FieldNet { base: args[0].clone() },
                about: String::new(),
                training: serde_json::Value::Null,
            },
            net: FieldNetwork::FieldNet(net),
        }
    };
    let series = std::env::var("FIELDS_SERIES").map(PathBuf::from).ok();
    let order_ab = std::env::var("FIELDS_ORDER").is_ok_and(|v| v == "ab");
    let resize: usize = std::env::var("FIELDS_RESIZE").ok().and_then(|s| s.parse().ok()).unwrap_or(1);
    let truth_dir = if std::env::var("FIELDS_TRUTH").is_ok_and(|v| v == "2") {
        "label_masks/semantic_2class"
    } else {
        "label_masks/semantic_3class"
    };
    println!("windows {}, resize {resize}, truth {truth_dir}", if order_ab { "A then B" } else { "B then A" });
    println!("{} on {name}; watershed seeds {seeds} %", args[0]);
    println!(
        "{:<18} {:>5} | {:>6} {:>6} {:>6} | {:>6} {:>6} | {:>6} {:>6} | {:>6} {:>6}",
        "set", "chips", "pxIoU", "pxP", "pxR", "objP", "objR", "intP", "intR", "wsP", "wsR"
    );
    for dir in &args[1..] {
        let dir = PathBuf::from(dir);
        let mut names: Vec<String> = std::fs::read_dir(dir.join("s2_images/window_a"))
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            .filter(|n| n.ends_with(".tif"))
            .collect();
        names.sort();
        let (mut px, mut obj, mut int, mut ws) =
            (Counts::default(), Counts::default(), Counts::default(), Counts::default());
        let t0 = std::time::Instant::now();
        for n in &names {
            let set = dir.file_name().unwrap().to_string_lossy().into_owned();
            let (_, _, w, h) = read_bands(&dir.join("label_masks/semantic_2class").join(n));
            let (p, dist): (Vec<f32>, Vec<u8>) = match (&model.net, &model.header.arch) {
                (FieldNetwork::FieldNet(net), _) => {
                    let (a, ca, _, _) = read_bands(&dir.join("s2_images/window_a").join(n));
                    let (b, cb, _, _) = read_bands(&dir.join("s2_images/window_b").join(n));
                    assert!(ca == 4 && cb == 4, "{n}: 4 bands a window");
                    // FTW's datasets stack window B then A (FIELDS_ORDER=ab: A then B)
                    let (first, second) = if order_ab { (&a, &b) } else { (&b, &a) };
                    let x: Vec<f32> = first.iter().chain(second).map(|v| v / fields::NORMALIZATION).collect();
                    let input = Tensor::<4>::from_data(TensorData::new(x, [1, 8, h, w]), &dev);
                    (fields::probabilities(net, input, resize).into_data().try_to_vec().unwrap(), vec![])
                }
                (FieldNetwork::Utae(net), FieldArch::Utae { standard, months, .. }) => {
                    let root = series.as_ref().expect("FIELDS_SERIES: the chips' monthly series");
                    let (x, b, _, _) = read_bands(&root.join(&set).join(n));
                    assert_eq!(b, months * 4, "{n}: {months} months of 4 bands");
                    let mut x: Vec<f32> =
                        x.iter().map(|&v| if v > 0.0 { v / fields::NORMALIZATION } else { f32::NAN }).collect();
                    standard.apply(&mut x, 4, w * h);
                    let input = Tensor::<5>::from_data(TensorData::new(x, [1, *months, 4, h, w]), &dev);
                    let pos: Vec<f32> = fields::month_positions(*months).iter().map(|&d| d as f32).collect();
                    let scores = net.forward(input, Tensor::<2>::from_data(TensorData::new(pos, [1, *months]), &dev));
                    let probs = burn::tensor::activation::softmax(scores.clone().slice([0..1, 0..3, 0..h, 0..w]), 1);
                    let d = burn::tensor::activation::sigmoid(scores.slice([0..1, 3..4, 0..h, 0..w]));
                    let d: Vec<f32> = d.into_data().try_to_vec().unwrap();
                    (
                        probs.into_data().try_to_vec().unwrap(),
                        d.iter().map(|v| (v * 100.0).round().clamp(0.0, 100.0) as u8).collect(),
                    )
                }
                _ => unreachable!(),
            };
            let m = w * h;
            // the class as ftw-tools takes it: the float scores' argmax
            let pred: Vec<u8> = (0..m)
                .map(|i| {
                    let v = [p[i], p[m + i], p[2 * m + i]];
                    let mut k = 0;
                    for j in 1..3 {
                        if v[j] > v[k] {
                            k = j;
                        }
                    }
                    k as u8
                })
                .collect();
            let (mut mask, _, _, _) = read_bands(&dir.join(truth_dir).join(n));
            if truth_dir.ends_with("3class") {
                // the boundary a negative, as ftw-tools tests on three classes
                for v in mask.iter_mut().filter(|v| **v == 2.0) {
                    *v = 0.0;
                }
            }
            let (inst, _, _, _) = read_bands(&dir.join("label_masks/instance").join(n));
            let mut c = Counts::default();
            for i in 0..m {
                let (t, q) = (mask[i] as u8, pred[i] == 1);
                if t == 3 {
                    continue;
                }
                match (t == 1, q) {
                    (true, true) => c.tp += 1,
                    (false, true) => c.fp += 1,
                    (true, false) => c.fn_ += 1,
                    _ => {}
                }
            }
            px.add(c);
            let truth_int = fields::components(&mask.iter().map(|&v| v as u8 == 1).collect::<Vec<_>>(), w, h);
            let pred_int = fields::components(&pred.iter().map(|&v| v == 1).collect::<Vec<_>>(), w, h);
            obj.add(match_objects(&truth_int, &pred_int));
            let probs = Probabilities {
                w,
                h,
                p: p.iter().map(|v| (v * 100.0).round().clamp(0.0, 100.0) as u8).collect(),
                dist,
            };
            let truth: Vec<u32> = inst.iter().map(|&v| v as u32).collect();
            int.add(match_objects(&truth, &fields::parcels(&probs, Parcels::Interiors, seeds)));
            ws.add(match_objects(&truth, &fields::parcels(&probs, Parcels::Watershed, seeds)));
        }
        let set = dir.file_name().unwrap().to_string_lossy().into_owned();
        println!(
            "{:<18} {:>5} | {:>6.2} {:>6.2} {:>6.2} | {:>6.2} {:>6.2} | {:>6.2} {:>6.2} | {:>6.2} {:>6.2}   ({:.1} s; {} fields)",
            set,
            names.len(),
            100.0 * px.iou(),
            100.0 * px.precision(),
            100.0 * px.recall(),
            100.0 * obj.precision(),
            100.0 * obj.recall(),
            100.0 * int.precision(),
            100.0 * int.recall(),
            100.0 * ws.precision(),
            100.0 * ws.recall(),
            t0.elapsed().as_secs_f64(),
            ws.tp + ws.fn_
        );
    }
}
