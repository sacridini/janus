//! Texture (Tools > Segmentation > Texture), as Zeit's texture: the shown series of a window summarized over its
//! dates (median, mean, minimum, maximum, standard deviation or amplitude), then zeit-core's `texture` of that image:
//! the grey-level co-occurrence matrix's properties (scikit-image's graycomatrix and graycoprops, averaged over four
//! directions) and spfeas's window mean, deviation and gradient. The windows need their neighbours, so a raster run
//! holds the window's values (eight bytes a value) and writes a map per feature.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use zeit_core::texture::{self, Feature, Options};

use crate::job::{ChunkOut, Inputs, JobResult, JobSpec, Outputs, active_outputs, rows_per_chunk};
use crate::manifest::Tool;
use crate::pixel::Params;

/// The window's values larger than this (eight bytes a value) are refused.
const MAX_BYTES: f64 = 6.0 * (1u64 << 30) as f64;

/// A pixel's summary of its values over the dates (NaN without any).
pub fn summarize(v: &mut Vec<f64>, how: &str) -> f64 {
    v.retain(|x| x.is_finite());
    if v.is_empty() {
        return f64::NAN;
    }
    let n = v.len() as f64;
    let q = |v: &mut Vec<f64>, p: f64| zeit_core::texture::quantile(v, p);
    match how {
        "mean" => v.iter().sum::<f64>() / n,
        "min" => v.iter().copied().fold(f64::INFINITY, f64::min),
        "max" => v.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        // xarray's std: the population's (ddof 0)
        "std" => {
            let m = v.iter().sum::<f64>() / n;
            (v.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / n).sqrt()
        }
        "amplitude" => q(v, 0.95) - q(v, 0.05),
        _ => q(v, 0.5),
    }
}

pub(crate) fn run(
    spec: &JobSpec,
    tool: &Tool,
    p: &Params,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<JobResult, String> {
    let mut inputs = Inputs::new(spec)?;
    let (iw, ih) = inputs.input.ds().raster_size();
    let window = spec.window.unwrap_or([0, 0, iw, ih]);
    let [x0, y0, x1, y1] = window;
    let (w, h) = (x1 - x0, y1 - y0);
    let (t, n) = (spec.years.len(), w * h);
    let outs_wanted = active_outputs(tool, p);
    let feats: Vec<Feature> = outs_wanted
        .iter()
        .map(|o| Feature::parse(&o.id))
        .collect::<Result<_, _>>()
        .map_err(|e| format!("ValueError: {e}"))?;
    if feats.is_empty() {
        return Err("ValueError: no feature chosen (the GLCM properties, the window statistics or both)".into());
    }
    let bytes = (8 * n * (2 + feats.len())) as f64;
    if bytes > MAX_BYTES {
        return Err(format!(
            "ValueError: the window's texture ({w} x {h} pixels, {} features) takes {:.1} GB; choose a smaller area",
            feats.len(),
            bytes / (1u64 << 30) as f64
        ));
    }
    // each pixel's summary of its dates, read a chunk of rows at a time
    let how = p.str("summary").to_string();
    let mut image = vec![f64::NAN; n];
    let rows = rows_per_chunk(tool, w);
    let mut r = 0;
    let mut v = Vec::with_capacity(t);
    while r < h {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let hh = rows.min(h - r);
        let chunk = inputs.read(x0, y0 + r, w, hh)?;
        let np = chunk.pixels();
        for k in 0..np {
            v.clear();
            v.extend((0..t).map(|i| chunk.stack[i * np + k]));
            image[r * w + k] = summarize(&mut v, &how);
        }
        r += hh;
        progress(0.3 * r as f64 / h as f64, &format!("reading rows {r}/{h}"));
    }
    let window_px = (p.int("window").max(3) as usize) | 1;
    let opts = Options {
        window: window_px,
        levels: p.int("levels").clamp(2, 254) as usize,
        distance: (p.int("distance").max(1) as usize).min(window_px - 1),
        lo: f64::NAN,
        hi: f64::NAN,
    };
    progress(0.3, &format!("the texture of the {how} in windows of {window_px} px"));
    let tex = texture::texture(&image, h, w, &feats, &opts, spec.n_jobs()).map_err(|e| format!("ValueError: {e}"))?;
    drop(image);
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    let dir = PathBuf::from(&spec.output_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut outs = Outputs::new(tool, &outs_wanted, &dir, inputs.input.ds(), window, spec)?;
    let mut r = 0;
    while r < h {
        let hh = rows.min(h - r);
        let (a, b) = (r * w, (r + hh) * w);
        let mut res = ChunkOut::new();
        for (fi, f) in feats.iter().enumerate() {
            res.insert(f.name().into(), tex[fi * n + a..fi * n + b].to_vec());
        }
        outs.write(&res, r, hh)?;
        r += hh;
        progress(0.9 + 0.1 * r as f64 / h as f64, &format!("writing rows {r}/{h}"));
    }
    let outputs = outs.describe(p, None);
    drop(outs);
    Ok(JobResult { outputs, window })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_are_xarrays() {
        let mut v = vec![1.0, f64::NAN, 3.0, 2.0, 10.0];
        assert_eq!(summarize(&mut v.clone(), "median"), 2.5);
        assert_eq!(summarize(&mut v.clone(), "max"), 10.0);
        assert_eq!(summarize(&mut v.clone(), "mean"), 4.0);
        assert!((summarize(&mut v, "std") - (12.5f64).sqrt()).abs() < 1e-12);
        assert!(summarize(&mut vec![f64::NAN], "median").is_nan());
    }
}
