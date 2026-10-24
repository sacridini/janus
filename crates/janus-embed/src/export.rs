//! A view at full resolution, for a file (src/embedding.cpp, computeEmbeddingExport): read from the source files
//! (not from the reduced store) over a window of the layer's pixels.

use std::sync::Arc;

use gdal::Dataset;
use janus_cube::CubeInfo;

use crate::classify::Classifier;
use crate::direction::DirectionBasis;
use crate::store::{BlockReader, band_info};
use crate::views::PcaBasis;
use crate::{parallel_for, thread_pool};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportKind {
    Pca,
    Similarity,
    Change,
    /// the change's principal directions (3 bands: the scores, NaN where it did not change)
    Direction,
    /// the change's likeness to a difference vector (`reference`)
    ChangeLike,
    /// the classes (2 bands: the class, 1 to k, NaN below the minimum score; the score)
    Classes,
}

pub struct EmbeddingExport {
    pub info: Arc<CubeInfo>,
    pub kind: ExportKind,
    /// the year; Change: and the year compared with
    pub t: usize,
    pub t0: Option<usize>,
    /// Pca: every component of it, one band each
    pub basis: Option<Arc<PcaBasis>>,
    /// Similarity: the reference vector (D values); ChangeLike: the difference vector
    pub reference: Vec<f32>,
    pub direction: Option<Arc<DirectionBasis>>,
    pub classifier: Option<Arc<Classifier>>,
    /// Direction, ChangeLike: the cosine distance above which a pixel changed; Classes: the minimum score
    pub threshold: f32,
    /// the store's dimensions without data, read as 0
    pub skip: Vec<u8>,
    /// x0, y0, x1, y1 in the layer's pixels
    pub win: [usize; 4],
    /// what the file holds, for the Exports window
    pub title: String,
    /// one per band
    pub band_names: Vec<String>,
    pub metadata: Vec<(String, String)>,
}

impl EmbeddingExport {
    pub fn bands(&self) -> usize {
        match self.kind {
            ExportKind::Pca => self.basis.as_ref().map_or(0, |b| b.k),
            ExportKind::Direction => 3,
            ExportKind::Classes => 2,
            _ => 1,
        }
    }
}

/// Computes e a block row at a time, in parallel: `rows(y, n, data)` gets n rows from row y of the window (data:
/// band after band, each n x width; NaN where a vector is missing) and returns false to stop. Err(why) on failure
/// (an empty why when stopped).
pub fn compute_embedding_export(
    e: &EmbeddingExport,
    threads: usize,
    rows: &mut dyn FnMut(usize, usize, &[f32]) -> bool,
) -> Result<(), String> {
    let info = &e.info;
    let (d, k) = (info.bands_per_date, e.bands());
    let [x0, y0, x1, y1] = e.win;
    if x1 <= x0 || y1 <= y0 || k == 0 {
        return Err("nothing to write".into());
    }
    let w = x1 - x0;
    if (e.kind == ExportKind::Pca && e.basis.as_ref().is_none_or(|b| b.mean.len() != d))
        || (matches!(e.kind, ExportKind::Similarity | ExportKind::ChangeLike) && e.reference.len() != d)
        || (e.kind == ExportKind::Direction && e.direction.as_ref().is_none_or(|b| b.mean.len() != d))
        || (e.kind == ExportKind::Classes && e.classifier.as_ref().is_none_or(|c| c.d != d))
    {
        return Err("the view does not match the layer's dimensions".into());
    }
    // one file per year (two for the change), read in their own type a block row at a time
    let pair = matches!(e.kind, ExportKind::Change | ExportKind::Direction | ExportKind::ChangeLike);
    let years: Vec<usize> = if pair { vec![e.t, e.t0.ok_or("no year to compare with")?] } else { vec![e.t] };
    let mut dss = Vec::new();
    for &t in &years {
        let l = info.layers.get(t).ok_or("no such year")?;
        let ds = Dataset::open(&l.path).map_err(|er| format!("{}: {er}", l.path))?;
        let (sw, sh) = ds.raster_size();
        if ds.raster_count() < l.band_base as usize + d || sw < x1 || sh < y1 {
            return Err(format!("{}: not the layer's grid or bands", l.path));
        }
        let bi = band_info(&ds, d, l.band_base as usize, &e.skip)?;
        dss.push((ds, bi, l.band_base as usize));
    }
    let mut rds: Vec<BlockReader> =
        dss.iter().map(|(ds, bi, base)| BlockReader::new(ds, bi, *base)).collect::<Result<_, _>>()?;
    // per output band: value = bias + sum_d W[d] v[d] (principal components: the centred projection; similarity:
    // the unit reference), then / |v| (similarity)
    let (mut wt, mut bias) = (vec![0f32; k * d], vec![0f32; k]);
    match e.kind {
        ExportKind::Pca => {
            let b = e.basis.as_ref().unwrap();
            for c in 0..k {
                for i in 0..d {
                    wt[c * d + i] = b.comps[c * d + i];
                    bias[c] -= b.mean[i] * wt[c * d + i];
                }
            }
        }
        ExportKind::Similarity => {
            let rn = e.reference.iter().map(|&v| f64::from(v) * f64::from(v)).sum::<f64>().sqrt();
            for (w, &r) in wt.iter_mut().zip(&e.reference) {
                *w = if rn > 0.0 { (f64::from(r) / rn) as f32 } else { 0.0 };
            }
        }
        _ => {}
    }
    let pool = thread_pool(threads);
    let bh = rds[0].bh;
    let mut y = y0;
    while y < y1 {
        let n = (bh - y % bh).min(y1 - y); // whole blocks of the file
        for (rd, &t) in rds.iter_mut().zip(&years) {
            rd.read(x0, y, w, n).map_err(|er| format!("{}: {er}", info.layers[t].path))?;
        }
        let plane = w * n;
        let out = std::sync::Mutex::new(vec![f32::NAN; plane * k]);
        let wins: Vec<_> = rds.iter().map(|r| r.window()).collect();
        parallel_for(&pool, plane, 4096, &|lo, hi| {
            let (mut a, mut c) = (vec![0f32; d], vec![0f32; d]);
            let mut local: Vec<(usize, f32)> = Vec::new();
            for i in lo..hi {
                let (px, py) = (x0 + i % w, y + i / w);
                wins[0].vector(px, py, &mut a);
                if !a.iter().all(|v| v.is_finite()) {
                    continue;
                }
                let na: f64 = a.iter().map(|&v| f64::from(v) * f64::from(v)).sum();
                if e.kind == ExportKind::Classes {
                    let c = e.classifier.as_ref().unwrap();
                    let (cl, score) = c.predict(&a);
                    if score >= e.threshold {
                        local.push((i, (cl + 1) as f32));
                    }
                    local.push((plane + i, score));
                    continue;
                }
                if matches!(e.kind, ExportKind::Direction | ExportKind::ChangeLike) {
                    wins[1].vector(px, py, &mut c);
                    if !c.iter().all(|v| v.is_finite()) {
                        continue;
                    }
                    let nc: f64 = c.iter().map(|&v| f64::from(v) * f64::from(v)).sum();
                    let dot: f64 = a.iter().zip(&c).map(|(&x, &y)| f64::from(x) * f64::from(y)).sum();
                    if na <= 0.0 || nc <= 0.0 || 1.0 - dot / (na * nc).sqrt() <= f64::from(e.threshold) {
                        continue;
                    }
                    let delta: Vec<f64> = a.iter().zip(&c).map(|(&x, &y)| f64::from(x) - f64::from(y)).collect();
                    if e.kind == ExportKind::ChangeLike {
                        let rn: f64 = e.reference.iter().map(|&r| f64::from(r) * f64::from(r)).sum::<f64>().sqrt();
                        let dn: f64 = delta.iter().map(|v| v * v).sum::<f64>().sqrt();
                        let s: f64 = delta.iter().zip(&e.reference).map(|(x, &r)| x * f64::from(r)).sum();
                        if rn > 0.0 && dn > 0.0 {
                            local.push((i, (s / (rn * dn)) as f32));
                        }
                    } else {
                        let b = e.direction.as_ref().unwrap();
                        for kk in 0..3 {
                            let s: f64 = (0..d)
                                .map(|j| (delta[j] - f64::from(b.mean[j])) * f64::from(b.comps[kk * d + j]))
                                .sum();
                            local.push((kk * plane + i, s as f32));
                        }
                    }
                    continue;
                }
                if e.kind == ExportKind::Change {
                    wins[1].vector(px, py, &mut c);
                    if !c.iter().all(|v| v.is_finite()) {
                        continue;
                    }
                    let nc: f64 = c.iter().map(|&v| f64::from(v) * f64::from(v)).sum();
                    let dot: f64 = a.iter().zip(&c).map(|(&x, &y)| f64::from(x) * f64::from(y)).sum();
                    if na > 0.0 && nc > 0.0 {
                        local.push((i, (1.0 - dot / (na * nc).sqrt()) as f32));
                    }
                    continue;
                }
                for kk in 0..k {
                    let s = f64::from(bias[kk])
                        + wt[kk * d..(kk + 1) * d]
                            .iter()
                            .zip(&a)
                            .map(|(&x, &y)| f64::from(x) * f64::from(y))
                            .sum::<f64>();
                    if e.kind == ExportKind::Similarity {
                        if na > 0.0 {
                            local.push((i, (s / na.sqrt()) as f32));
                        }
                    } else {
                        local.push((kk * plane + i, s as f32));
                    }
                }
            }
            let mut o = out.lock().unwrap();
            for (i, v) in local {
                o[i] = v;
            }
        });
        if !rows(y - y0, n, &out.into_inner().unwrap()) {
            return Err(String::new());
        }
        y += n;
    }
    Ok(())
}
