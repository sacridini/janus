//! Emerging hot spot analysis, as Zeit's emerging_hotspots (ESRI's Space Time Pattern Mining, sfdep's
//! emerging_hotspot_analysis): each date's Getis-Ord Gi* z-scores, the Mann-Kendall trend of each pixel's Gi* and
//! its pattern (new, consecutive, intensifying, persistent, diminishing, sporadic, oscillating or historical hot or
//! cold spot). The neighbourhoods need the whole map, so a raster run holds the window's values (eight bytes a value)
//! and writes the patterns, the Gi* series (a new layer), its trend and the dates of hot and cold spots.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use zeit_core::hotspots::{self, Params as HotspotParams};
use zeit_core::mann_kendall::Method;

use crate::job::{ChunkOut, Inputs, JobResult, JobSpec, Outputs, active_outputs, rows_per_chunk};
use crate::manifest::Tool;
use crate::pixel::Params;

/// The window's values and their Gi* larger than this (about five copies of eight bytes a value) are refused.
const MAX_BYTES: f64 = 6.0 * (1u64 << 30) as f64;

pub fn params(p: &Params) -> HotspotParams {
    HotspotParams {
        radius: p.int("radius").max(1) as usize,
        circle: p.str("shape") == "circle",
        lag: p.int("lag").max(0) as usize,
        cube_reference: p.str("reference") == "cube",
        alpha: p.f64("alpha"),
        method: match p.str("trend") {
            "original" => Method::Original,
            "yue_wang" => Method::YueWang,
            _ => Method::HamedRao,
        },
    }
}

/// A pattern as the map's class (from 1, as Janus' class maps): 1 no pattern, 2-9 the hot spots (new ...
/// historical), 10-17 the cold ones.
pub fn class_of(code: i8) -> f64 {
    match code {
        hotspots::NONE => f64::NAN,
        c if c >= 0 => f64::from(c) + 1.0,
        c => 9.0 - f64::from(c),
    }
}

pub(crate) fn run(
    spec: &JobSpec,
    tool: &Tool,
    p: &Params,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<JobResult, String> {
    let hp = params(p);
    let mut inputs = Inputs::new(spec)?;
    let (iw, ih) = inputs.input.ds().raster_size();
    let window = spec.window.unwrap_or([0, 0, iw, ih]);
    let [x0, y0, x1, y1] = window;
    let (w, h) = (x1 - x0, y1 - y0);
    let (t, n) = (spec.years.len(), w * h);
    let bytes = (5 * 8 * t * n) as f64;
    if bytes > MAX_BYTES {
        return Err(format!(
            "ValueError: the window's values ({t} dates of {w} x {h} pixels) take {:.1} GB with their Gi*; choose \
             a smaller area",
            bytes / (1u64 << 30) as f64
        ));
    }
    // the values, [T][rows][columns]
    let mut values = vec![f64::NAN; t * n];
    let rows = rows_per_chunk(tool, w);
    let mut r = 0;
    while r < h {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let hh = rows.min(h - r);
        let chunk = inputs.read(x0, y0 + r, w, hh)?;
        let np = chunk.pixels();
        for i in 0..t {
            values[i * n + r * w..i * n + r * w + np].copy_from_slice(&chunk.stack[i * np..(i + 1) * np]);
        }
        r += hh;
        progress(0.3 * r as f64 / h as f64, &format!("reading rows {r}/{h}"));
    }
    progress(0.3, "Gi* of every date and the trends");
    let e = hotspots::emerging(&values, t, h, w, &hp, spec.n_jobs());
    drop(values);
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    let dir = PathBuf::from(&spec.output_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut outs = Outputs::new(tool, &active_outputs(tool, p), &dir, inputs.input.ds(), window, spec)?;
    let mut r = 0;
    while r < h {
        let hh = rows.min(h - r);
        let (a, b) = (r * w, (r + hh) * w);
        let mut res = ChunkOut::new();
        let none = |k: usize| e.category[k] == hotspots::NONE;
        res.insert("pattern".into(), (a..b).map(|k| class_of(e.category[k])).collect());
        let mut gi = Vec::with_capacity(t * (b - a));
        for i in 0..t {
            gi.extend_from_slice(&e.gi[i * n + a..i * n + b]);
        }
        res.insert("gi".into(), gi);
        res.insert("trend_z".into(), e.trend_z[a..b].to_vec());
        res.insert("n_hot".into(), (a..b).map(|k| if none(k) { f64::NAN } else { f64::from(e.n_hot[k]) }).collect());
        res.insert("n_cold".into(), (a..b).map(|k| if none(k) { f64::NAN } else { f64::from(e.n_cold[k]) }).collect());
        outs.write(&res, r, hh)?;
        r += hh;
        progress(0.9 + 0.1 * r as f64 / h as f64, &format!("writing rows {r}/{h}"));
    }
    let outputs = outs.describe(p, None);
    drop(outs);
    Ok(JobResult { outputs, window })
}
