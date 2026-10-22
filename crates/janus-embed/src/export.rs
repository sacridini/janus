//! A view at full resolution, for a file (src/embedding.cpp, computeEmbeddingExport): read from the source files
//! (not from the reduced store) over a window of the layer's pixels.

use std::sync::Arc;

use gdal::Dataset;
use janus_cube::CubeInfo;

use crate::store::{BlockReader, band_info};
use crate::views::PcaBasis;
use crate::{parallel_for, thread_pool};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportKind {
    Pca,
    Similarity,
    Change,
}

pub struct EmbeddingExport {
    pub info: Arc<CubeInfo>,
    pub kind: ExportKind,
    /// the year; Change: and the year compared with
    pub t: usize,
    pub t0: Option<usize>,
    /// Pca: every component of it, one band each
    pub basis: Option<Arc<PcaBasis>>,
    /// Similarity: the reference vector (D values)
    pub reference: Vec<f32>,
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
        if self.kind == ExportKind::Pca { self.basis.as_ref().map_or(0, |b| b.k) } else { 1 }
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
        || (e.kind == ExportKind::Similarity && e.reference.len() != d)
    {
        return Err("the view does not match the layer's dimensions".into());
    }
    // one file per year (two for the change), read in their own type a block row at a time
    let years: Vec<usize> =
        if e.kind == ExportKind::Change { vec![e.t, e.t0.ok_or("no year to compare with")?] } else { vec![e.t] };
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
        ExportKind::Change => {}
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
