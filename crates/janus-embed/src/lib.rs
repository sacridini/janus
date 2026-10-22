//! Foundation-model embeddings (TESSERA, AlphaEarth...; src/embedding.cpp): one file per year with D bands, each
//! pixel a D-dimensional vector with no physical unit. Janus shows them through their principal components (RGB),
//! the cosine similarity to a reference vector, or the change from one year to the next; everything is computed on
//! the CPU from a reduced copy of every year (EmbeddingStore) and drawn as an image.

// the kernels and loops index their arrays as the C++ does (several arrays at once)
#![allow(clippy::needless_range_loop)]

pub mod eigen;
pub mod engine;
pub mod export;
pub mod selftest;
pub mod simd;
pub mod store;
pub mod views;

use janus_cube::CubeInfo;
use rayon::ThreadPool;
use rayon::prelude::*;
use serde_json::Value;

pub use engine::EmbeddingEngine;
pub use export::{EmbeddingExport, ExportKind, compute_embedding_export};
pub use store::EmbeddingStore;
pub use views::{
    PcaBasis, fit_pca, percentile_range, render_change, render_pca_rgb, render_similarity, sample_vectors,
};

/// What the layer holds, from Zeit's ZEIT_EMBEDDING tag (written by Janus' download and by zeit.save_raster) or,
/// without it, from its bands (16+ bands per date named A00, A01...).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EmbeddingMeta {
    pub is: bool,
    /// "alphaearth", "tessera" ("" = recognised by its bands only)
    pub source: String,
    pub model: String,
    pub version: String,
    pub license: String,
    pub attribution: String,
    pub dims: usize,
}

impl EmbeddingMeta {
    /// e.g. "AlphaEarth Foundations, 64 dimensions"
    pub fn label(&self) -> String {
        let mut s = if !self.model.is_empty() && !self.model.starts_with("http") {
            self.model.clone()
        } else {
            match self.source.as_str() {
                "tessera" => "TESSERA".into(),
                "alphaearth" => "AlphaEarth Foundations".into(),
                "" => "Embeddings".into(),
                other => other.into(),
            }
        };
        if self.source == "tessera" && !self.version.is_empty() {
            s += &format!(" v{}", self.version);
        }
        format!("{s}, {} dimensions", self.dims)
    }
}

pub fn embedding_meta(info: &CubeInfo) -> EmbeddingMeta {
    let mut m = EmbeddingMeta::default();
    if info.bands_per_date < 2 {
        return m;
    }
    if !info.embedding_tag.is_empty()
        && let Ok(j @ Value::Object(_)) = serde_json::from_str::<Value>(&info.embedding_tag)
    {
        let text = |k: &str| match &j[k] {
            Value::Null => String::new(),
            Value::String(s) => s.clone(),
            v => v.to_string(),
        };
        m.is = true;
        (m.source, m.model, m.version) = (text("embedding_source"), text("embedding_model"), text("embedding_version"));
        (m.license, m.attribution) = (text("embedding_license"), text("embedding_attribution"));
        m.dims = text("embedding_dimensions").parse().unwrap_or(0);
    }
    if !m.is && info.bands_per_date >= 16 {
        // Zeit's dimension names: A00, A01...
        let named = info
            .band_names
            .iter()
            .filter(|n| n.len() >= 2 && n.starts_with('A') && n[1..].bytes().all(|c| c.is_ascii_digit()))
            .count();
        m.is = named == info.bands_per_date;
    }
    if m.is && m.dims == 0 {
        m.dims = info.bands_per_date;
    }
    m
}

/// A pool of `threads` threads for the views.
pub fn thread_pool(threads: usize) -> ThreadPool {
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads.max(1))
        .thread_name(|i| format!("embeddings-{i}"))
        .build()
        .expect("a thread pool")
}

/// Runs f(begin, end) over [0, n) split among the pool's threads; returns when all are done.
pub fn parallel_for(pool: &ThreadPool, n: usize, grain: usize, f: &(dyn Fn(usize, usize) + Sync)) {
    if n == 0 {
        return;
    }
    let max_chunks = pool.current_num_threads() * 4;
    let chunks = n.div_ceil(grain.max(1)).min(max_chunks).max(1);
    if chunks == 1 {
        f(0, n);
        return;
    }
    pool.install(|| (0..chunks).into_par_iter().for_each(|c| f(n * c / chunks, n * (c + 1) / chunks)));
}
