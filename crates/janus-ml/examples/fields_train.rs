//! Trains a field model on labelled chips (WRI Trazo's or Fields of The World's layout: label_masks/semantic_3class
//! and instance, s2_images/window_a and window_b) and keeps it as a Janus field model:
//!
//! - `utae`: a U-TAE on each chip's monthly composites (SERIES/<set>/<chip>.tif: 4 bands a month, red, green, blue,
//!   NIR, digital numbers of baseline 03, 0 none, from July of the year before the labels' year), three classes
//!   and the distance to the field's edge;
//! - `fieldnet`: the FTW/Trazo U-Net (FIELDS_BASE, a checkpoint) fine-tuned on the chips' two windows.
//!
//!     cargo run --release --example fields_train -- utae|fieldnet OUT.janusfields SERIES DIR [DIR ...]
//!
//! (JANUS_ML_GPU=1 for the GPU; FIELDS_EPOCHS, FIELDS_BATCH, FIELDS_CROP, FIELDS_LR, FIELDS_PATIENCE.)

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use gdal::Dataset;
use janus_ml::dense::NONE;
use janus_ml::fieldnet::{self, FieldNet};
use janus_ml::fields_train::{self as ft, Chip, FieldArch, FieldHeader, FieldModel, FieldNetwork, FieldTrainConfig};
use janus_ml::model::device;
use janus_ml::utae::UtaeHyper;

fn read<T: gdal::raster::GdalType + Copy>(path: &Path) -> Option<(Vec<T>, usize, usize, usize)> {
    let ds = Dataset::open(path).ok()?;
    let (w, h) = ds.raster_size();
    let n = ds.raster_count();
    let mut v = Vec::with_capacity(n * w * h);
    for b in 1..=n {
        v.extend(ds.rasterband(b).ok()?.read_as::<T>((0, 0), (w, h), (w, h), None).ok()?.data());
    }
    Some((v, n, w, h))
}

fn env<T: std::str::FromStr>(k: &str, d: T) -> T {
    std::env::var(k).ok().and_then(|s| s.parse().ok()).unwrap_or(d)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 4 || !["utae", "fieldnet"].contains(&args[0].as_str()) {
        eprintln!("usage: fields_train utae|fieldnet OUT SERIES DIR [DIR ...]");
        std::process::exit(2);
    }
    let temporal = args[0] == "utae";
    let (out, series) = (PathBuf::from(&args[1]), PathBuf::from(&args[2]));
    let t0 = std::time::Instant::now();
    let mut chips = vec![];
    let mut sets = vec![];
    for dir in &args[3..] {
        let dir = PathBuf::from(dir);
        let set = dir.file_name().unwrap().to_string_lossy().into_owned();
        let mut names: Vec<String> = std::fs::read_dir(dir.join("label_masks/instance"))
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            .filter(|n| n.ends_with(".tif"))
            .collect();
        names.sort();
        let before = chips.len();
        for n in &names {
            let Some((cls, _, w, h)) = read::<u8>(&dir.join("label_masks/semantic_3class").join(n)) else { continue };
            let Some((inst, _, _, _)) = read::<u32>(&dir.join("label_masks/instance").join(n)) else { continue };
            if w != h {
                continue;
            }
            let x = if temporal {
                let Some((x, b, xw, xh)) = read::<u16>(&series.join(&set).join(n)) else { continue };
                if (xw, xh) != (w, h) || b % 4 != 0 {
                    continue;
                }
                (x, b / 4, 4)
            } else {
                let Some((a, ca, _, _)) = read::<u16>(&dir.join("s2_images/window_a").join(n)) else { continue };
                let Some((b, cb, _, _)) = read::<u16>(&dir.join("s2_images/window_b").join(n)) else { continue };
                if ca != 4 || cb != 4 {
                    continue;
                }
                // window B first, as the FTW and Trazo datasets stack them
                ([b, a].concat(), 1, 8)
            };
            chips.push(Chip {
                t: x.1,
                c: x.2,
                side: w,
                x: x.0,
                classes: cls.iter().map(|&v| if v <= 2 { v } else { NONE }).collect(),
                dist: ft::distance_to_edge(&inst, w, h),
            });
        }
        println!("{set}: {} chips", chips.len() - before);
        sets.push(format!("{set} ({})", chips.len() - before));
    }
    let shapes: std::collections::BTreeSet<(usize, usize)> = chips.iter().map(|c| (c.t, c.c)).collect();
    assert!(shapes.len() == 1, "the chips' steps and values differ: {shapes:?}");
    println!("{} chips read in {:.0} s", chips.len(), t0.elapsed().as_secs_f64());
    let cfg = FieldTrainConfig {
        epochs: env("FIELDS_EPOCHS", 60),
        batch: env("FIELDS_BATCH", 4),
        crop: env("FIELDS_CROP", if temporal { 128 } else { 256 }),
        lr: env("FIELDS_LR", if temporal { 1e-3 } else { 1e-4 }),
        patience: env("FIELDS_PATIENCE", 10),
        ..Default::default()
    };
    let (dev, name) = device(std::env::var_os("JANUS_ML_GPU").is_some());
    println!("training {} on {name}: {cfg:?}", args[0]);
    let cancel = AtomicBool::new(false);
    let mut on_epoch = |e: &janus_ml::train::Epoch| {
        println!(
            "epoch {:>3}: loss {:.4} acc {:.3} | val loss {:.4} acc {:.3} ({:.0} s)",
            e.epoch, e.train_loss, e.train_acc, e.val_loss, e.val_acc, e.seconds
        )
    };
    let model = if temporal {
        let t = chips[0].t;
        let standard = ft::standard_of(&chips, 7);
        let hyper = UtaeHyper { patch: cfg.crop, ..UtaeHyper::default() };
        let days = janus_ml::fields::month_positions(t);
        let (net, trained) = ft::train_field_utae(
            &chips,
            &|d| Ok(ft::field_utae(&hyper, 4, d)),
            &days,
            &standard,
            &cfg,
            &dev,
            &mut on_epoch,
            &cancel,
        )
        .unwrap();
        FieldModel {
            header: FieldHeader {
                arch: FieldArch::Utae { hyper, months: t, first_month: 7, standard },
                about: format!("U-TAE of fields on WRI Trazo's annotations: {}", sets.join(", ")),
                training: serde_json::json!({"config": cfg, "result": trained}),
            },
            net: FieldNetwork::Utae(net),
        }
    } else {
        let base = std::env::var("FIELDS_BASE").expect("FIELDS_BASE: the checkpoint to start from");
        let start = |d: &burn::prelude::Device| {
            FieldNet::new(fieldnet::INPUTS, fieldnet::CLASSES, d).load_ftw(Path::new(&base))
        };
        let (net, trained) = ft::train_fieldnet(&chips, &start, &cfg, &dev, &mut on_epoch, &cancel).unwrap();
        FieldModel {
            header: FieldHeader {
                arch: FieldArch::FieldNet { base: base.clone() },
                about: format!("{base} fine-tuned on {}", sets.join(", ")),
                training: serde_json::json!({"config": cfg, "result": trained}),
            },
            net: FieldNetwork::FieldNet(net),
        }
    };
    model.save(&out).unwrap();
    println!("kept {} ({:.0} s in all)", out.display(), t0.elapsed().as_secs_f64());
}
