//! Post-processing of a classification (a categorical layer: one map or a map per date), as Zeit's: each pixel's
//! short excursions removed (A B A -> A A A, zeit.desawtooth_classes), then each date's map through a majority
//! filter (zeit.apply_majority_filter) and a minimum mapping unit that merges the small patches into their
//! neighbours (zeit.mmu_sieve). The spatial filters need the whole map, so a raster run holds the window's classes
//! (two bytes a value) and writes them, cleaned, as a series of classes with the layer's names and colours.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use zeit_core::postproc;

use crate::job::{ChunkOut, Inputs, JobResult, JobSpec, Outputs, active_outputs, rows_per_chunk};
use crate::manifest::Tool;
use crate::pixel::{Overlay, Params, PixelInput, PixelResult, row};

/// The window's classes larger than this (two bytes a value, twice) are refused: a smaller window is asked for.
const MAX_BYTES: f64 = 4.0 * (1u64 << 30) as f64;

/// A missing class in the window's store.
const NONE: u16 = u16::MAX;

/// The filters a run applies (0: off).
struct Filters {
    max_length: usize,
    window: usize,
    iterations: usize,
    min_size: usize,
    eight: bool,
}

impl Filters {
    fn of(p: &Params) -> Filters {
        let window = p.int("majority").max(0) as usize;
        Filters {
            max_length: p.int("max_length").max(0) as usize,
            // an even window is made odd (one more pixel)
            window: if window >= 2 { window | 1 } else { 0 },
            iterations: p.int("iterations").max(1) as usize,
            min_size: p.int("min_size").max(0) as usize,
            eight: p.str("connectivity") != "4",
        }
    }

    /// The temporal filter of pixels' series (pixels x dates).
    fn temporal(&self, series: &[f64], n: usize, t: usize, n_jobs: i32) -> Vec<f64> {
        if self.max_length == 0 || t < 3 {
            return series.to_vec();
        }
        postproc::desawtooth(series, n, t, self.max_length, n_jobs)
    }

    /// The spatial filters of one date's map.
    fn spatial(&self, map: Vec<f64>, h: usize, w: usize, n_jobs: i32) -> Vec<f64> {
        let map =
            if self.window >= 3 { postproc::majority(&map, h, w, self.window, self.iterations, n_jobs) } else { map };
        if self.min_size >= 2 { postproc::sieve(&map, h, w, self.min_size, self.eight) } else { map }
    }
}

/// A pixel: the temporal filter (the spatial ones need the neighbours: raster runs).
pub fn pixel(p: &Params, input: &PixelInput) -> Result<PixelResult, String> {
    let f = Filters::of(p);
    let t = input.values.len();
    let cleaned = f.temporal(&input.values, 1, t, 1);
    let changed: Vec<usize> =
        (0..t).filter(|&i| input.values[i].is_finite() && cleaned[i] != input.values[i]).collect();
    let mut overlays = vec![];
    if !changed.is_empty() {
        let mut line = Overlay::new(
            "line",
            "Classes after the filter",
            input.years.clone(),
            cleaned.iter().map(|&v| v.is_finite().then_some(v)).collect(),
        );
        line.weight = Some(2.0);
        overlays.push(line);
        overlays.push(Overlay::new(
            "markers",
            "Changed by the filter",
            changed.iter().map(|&i| input.years[i]).collect(),
            changed.iter().map(|&i| Some(cleaned[i])).collect(),
        ));
    }
    let temporal = if f.max_length == 0 {
        "off".to_string()
    } else if t < 3 {
        "needs 3 dates or more".to_string()
    } else {
        format!("runs of up to {} date(s) between two of one class", f.max_length)
    };
    let mut rows = vec![row("Temporal filter", temporal), row("Dates changed", changed.len().to_string())];
    let spatial = match (f.window >= 3, f.min_size >= 2) {
        (false, false) => "off".to_string(),
        (m, s) => {
            let mut parts = vec![];
            if m {
                parts.push(format!("majority {0} x {0} ({1} time(s))", f.window, f.iterations));
            }
            if s {
                parts.push(format!("min. mapping unit {} px ({}-connected)", f.min_size, if f.eight { 8 } else { 4 }));
            }
            format!("{}: on the map (raster runs)", parts.join(", "))
        }
    };
    rows.push(row("Spatial filters", spatial));
    Ok(PixelResult { overlays, rows })
}

/// A class as stored: a whole number from 0 to 65534, else an error.
fn class_of(v: f64) -> Result<u16, String> {
    if v.is_nan() {
        return Ok(NONE);
    }
    if v.fract() != 0.0 || !(0.0..65535.0).contains(&v) {
        return Err(format!(
            "ValueError: the layer holds {v}, not a class: post-processing takes classes, whole numbers from 0 to \
             65534 (a categorical layer)"
        ));
    }
    Ok(v as u16)
}

fn value_of(c: u16) -> f64 {
    if c == NONE { f64::NAN } else { f64::from(c) }
}

pub(crate) fn run(
    spec: &JobSpec,
    tool: &Tool,
    p: &Params,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<JobResult, String> {
    let f = Filters::of(p);
    let mut inputs = Inputs::new(spec)?;
    let (iw, ih) = inputs.input.ds().raster_size();
    let window = spec.window.unwrap_or([0, 0, iw, ih]);
    let [x0, y0, x1, y1] = window;
    let (w, h) = (x1 - x0, y1 - y0);
    let (t, n) = (spec.years.len(), w * h);
    let bytes = (2 * 2 * t * n) as f64;
    if bytes > MAX_BYTES {
        return Err(format!(
            "ValueError: the window's classes ({t} dates of {w} x {h} pixels) take {:.1} GB; choose a smaller area",
            bytes / (1u64 << 30) as f64
        ));
    }
    let n_jobs = spec.n_jobs();
    // the classes as they were and as filtered, [T][n]
    let mut before = vec![NONE; t * n];
    let mut after = vec![NONE; t * n];
    let rows = rows_per_chunk(tool, w);
    let mut r = 0;
    while r < h {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let hh = rows.min(h - r);
        let chunk = inputs.read(x0, y0 + r, w, hh)?;
        let np = chunk.pixels();
        let cleaned = f.temporal(&chunk.by_pixel(&chunk.stack), np, t, n_jobs);
        for px in 0..np {
            for i in 0..t {
                let at = i * n + r * w + px;
                before[at] = class_of(chunk.stack[i * np + px])?;
                after[at] = class_of(cleaned[px * t + i])?;
            }
        }
        r += hh;
        progress(0.3 * r as f64 / h as f64, &format!("reading rows {r}/{h}"));
    }
    if f.window >= 3 || f.min_size >= 2 {
        for i in 0..t {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            let date = &mut after[i * n..(i + 1) * n];
            let map = f.spatial(date.iter().map(|&c| value_of(c)).collect(), h, w, n_jobs);
            for (c, v) in date.iter_mut().zip(map) {
                *c = class_of(v)?;
            }
            progress(0.3 + 0.6 * (i + 1) as f64 / t as f64, &format!("filtering the map of date {}/{t}", i + 1));
        }
    }
    let dir = PathBuf::from(&spec.output_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut outs = Outputs::new(tool, &active_outputs(tool, p), &dir, inputs.input.ds(), window, spec)?;
    let mut r = 0;
    while r < h {
        let hh = rows.min(h - r);
        let (a, b) = (r * w, (r + hh) * w);
        let mut res = ChunkOut::new();
        let mut series = Vec::with_capacity(t * (b - a));
        for i in 0..t {
            series.extend(after[i * n + a..i * n + b].iter().map(|&c| value_of(c)));
        }
        res.insert("classes".into(), series);
        res.insert(
            "changed".into(),
            (a..b)
                .map(|k| {
                    if (0..t).all(|i| before[i * n + k] == NONE) {
                        f64::NAN
                    } else {
                        (0..t).filter(|&i| before[i * n + k] != after[i * n + k]).count() as f64
                    }
                })
                .collect(),
        );
        outs.write(&res, r, hh)?;
        r += hh;
        progress(0.9 + 0.1 * r as f64 / h as f64, &format!("writing rows {r}/{h}"));
    }
    let outputs = outs.describe(p, None);
    drop(outs);
    Ok(JobResult { outputs, window })
}
