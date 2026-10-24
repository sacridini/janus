//! Foundation-model embeddings in the interface (src/app_embeddings.cpp): a layer of embeddings is drawn through
//! its principal components (RGB), the similarity to a reference or the change between years, computed on the CPU
//! in the background (janus_embed) and drawn as an image; the Embeddings panel; the views written as GeoTIFFs.
//! A spectrum's bands are one vector per pixel: its principal components and spectral similarity.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use egui::{Color32, Pos2, Rect, Ui, vec2};
use egui_plot::{Line, Plot, PlotPoints, VLine};
use janus_cube::CubeInfo;
use janus_embed::classify::{Classifier, Method, NONE, classify_year, fit, layer_norm};
use janus_embed::direction::change_vector;
use janus_embed::{
    DirectionBasis, EmbeddingEngine, EmbeddingExport, EmbeddingMeta, EmbeddingStore, ExportKind, PcaBasis,
    embedding_meta, fit_direction, fit_pca, percentile_range, render_change, render_change_like, render_direction,
    render_pca_rgb, render_similarity, sample_vectors,
};
use janus_geo::plane;
use janus_gpu::GpuTexture;

use crate::app::State;
use crate::exports::{ExportJob, start_export_job};
use crate::map::body_font;
use crate::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    /// the band values (Display panel)
    Band = 0,
    Pca,
    Similarity,
    Change,
    /// the change's principal directions (RGB), or its likeness to a pin's
    Direction,
    /// few-shot classes from pins and the ROI
    Classes,
}

const VIEW_NAMES: [&str; 6] = [
    "Band values (Display panel)",
    "Principal components (RGB)",
    "Similarity to a reference",
    "Change between years",
    "Direction of change",
    "Classes from pins",
];

/// A class of the few-shot classification: its name, colour and samples (vectors of the layer) with where each
/// came from.
#[derive(Clone)]
pub struct EmbClass {
    pub name: String,
    pub color: Color32,
    pub samples: Vec<Vec<f32>>,
    pub sources: Vec<String>,
}

/// The colours of new classes.
const CLASS_COLOURS: [Color32; 10] = [
    Color32::from_rgb(34, 139, 34),
    Color32::from_rgb(30, 144, 255),
    Color32::from_rgb(218, 165, 32),
    Color32::from_rgb(220, 20, 60),
    Color32::from_rgb(148, 103, 189),
    Color32::from_rgb(140, 86, 75),
    Color32::from_rgb(227, 119, 194),
    Color32::from_rgb(127, 127, 127),
    Color32::from_rgb(188, 189, 34),
    Color32::from_rgb(23, 190, 207),
];

/// Where a class's samples come from.
#[derive(Clone, Copy)]
pub(crate) enum ClassSource {
    Pin(u32),
    Roi,
}

fn mix(h: u64, v: u64) -> u64 {
    ((h ^ v).wrapping_mul(0x0100_0000_01b3)).wrapping_add(h >> 29).wrapping_add(0x9e37_79b9_7f4a_7c15)
}

/// The main map's title, colour bar (colormap, ends) and wait message of a layer drawn as embeddings.
pub(crate) type MapInfo = (String, Option<(usize, f32, f32)>, String);

/// A float view's range: automatic (from the first image of each reference or base) or set by hand.
#[derive(Clone, Copy, Default)]
pub struct ViewRange {
    pub lo: f32,
    pub hi: f32,
    pub manual: bool,
    /// what the automatic range was taken from (0: none yet)
    key: u64,
}

/// A year's image: the last one made (its key says for what).
#[derive(Default)]
pub struct Img {
    tex: Option<GpuTexture>,
    pub key: u64,
    pub rgba: bool,
    /// float views: kept for the value under the cursor (classes: the score)
    pub values: Vec<f32>,
    /// classes: each pixel's class (NONE: none), and the pixels of each class then those below the minimum score
    pub classes: Vec<u8>,
    pub counts: Vec<usize>,
    /// its automatic range
    pub lo: f32,
    pub hi: f32,
    pub ms: f64,
}

impl Img {
    pub fn has_texture(&self) -> bool {
        self.tex.is_some()
    }
}

/// An image made by the engine.
struct Done {
    t: usize,
    view: View,
    key: u64,
    rgba: Vec<u8>,
    values: Vec<f32>,
    classes: Vec<u8>,
    counts: Vec<usize>,
    lo: f32,
    hi: f32,
    ms: f64,
}

/// What the engine's thread posts.
#[derive(Default)]
struct Results {
    /// a new basis: its key, from the ROI
    basis: Option<(Arc<PcaBasis>, u64, bool)>,
    direction: Option<(Arc<DirectionBasis>, u64)>,
    classifier: Option<(Result<Arc<Classifier>, String>, u64)>,
    done: Vec<Done>,
}

pub struct EmbeddingLayer {
    pub meta: EmbeddingMeta,
    /// a spectrum (hyperspectral): its bands are one vector per pixel, a single "year" (info)
    pub spectral: bool,
    /// what the store reads: the layer's series, or that "year"
    pub info: Arc<CubeInfo>,
    pub store: Option<Arc<EmbeddingStore>>,
    engine: Option<EmbeddingEngine>,
    pub view: View,
    /// principal components: 0 every pixel of every year, 1 the ROI (local PCA)
    pub scope: usize,
    /// drawn as R, G, B
    pub comps: [usize; 3],
    /// percent clipped at each end
    pub stretch: f32,
    pub basis: Option<Arc<PcaBasis>>,
    pub basis_have: u64,
    pub basis_sent: u64,
    pub basis_local: bool,
    /// similarity: 0 the cursor, 1 a pin, 2 the ROI mean
    pub ref_kind: usize,
    pub ref_pin: u32,
    pub ref_fixed_year: bool,
    pub ref_year: usize,
    /// the dequantised reference vector
    pub reference: Vec<f32>,
    ref_key: u64,
    pub ref_text: String,
    /// change: None the previous year, else a fixed year
    pub change_base: Option<usize>,
    /// direction of change: 0 its principal directions (RGB), 1 like a pin's change
    pub dir_mode: usize,
    pub dir_pin: u32,
    /// the pin's change of this year (and the year compared with), else of the year shown
    pub dir_ref_year: Option<usize>,
    /// the cosine distance above which a pixel changed: automatic (the layer's unusual changes), else set
    pub dir_auto: bool,
    pub dir_threshold: f32,
    pub dir_basis: Option<Arc<DirectionBasis>>,
    dir_have: u64,
    dir_sent: u64,
    pub dir_ref: Vec<f32>,
    dir_ref_key: u64,
    pub dir_ref_text: String,
    /// few-shot classes
    pub classes: Vec<EmbClass>,
    /// 0 logistic regression, 1 nearest centroid (cosine)
    pub cls_method: usize,
    /// the logistic regression's L2 penalty
    pub cls_lambda: f32,
    /// below this score (probability, cosine) a pixel has no class
    pub cls_min: f32,
    pub classifier: Option<Arc<Classifier>>,
    pub cls_error: String,
    /// changed with the classes, their samples or the method
    pub cls_gen: u64,
    cls_have: u64,
    cls_sent: u64,
    /// the year the transitions to the year shown are counted from
    pub cls_from: Option<usize>,
    pub range: [ViewRange; 6],
    pub cmap: [usize; 6],
    pub img: Vec<Img>,
    slot_sent: HashMap<&'static str, u64>,
    years_seen: usize,
    results: Arc<Mutex<Results>>,
}

impl EmbeddingLayer {
    /// A layer of embeddings, or a spectrum's analysis (its bands as one vector); None for other series.
    pub fn new(series: &Arc<CubeInfo>) -> Option<Box<Self>> {
        let meta = embedding_meta(series);
        if !meta.is && !series.spectral {
            return None;
        }
        let (info, view) = if series.spectral {
            // the file's bands, in its order, as one vector: shown when picked in the panel (the bands until then)
            let mut one = (**series).clone();
            one.spectral = false;
            one.bands_per_date = series.band_nm.len();
            let mut y = series.layers[0].clone();
            (y.path, y.label, y.band_base) = (series.first_path.clone(), "spectrum".into(), 0);
            one.layers = vec![y];
            (Arc::new(one), View::Band)
        } else {
            (series.clone(), View::Pca)
        };
        let viridis = janus_gpu::colormaps::VIRIDIS;
        let plasma = janus_gpu::colormaps::by_name("Plasma").unwrap_or(viridis);
        let rdbu = janus_gpu::colormaps::by_name("RdBu").unwrap_or(viridis);
        Some(Box::new(EmbeddingLayer {
            meta,
            spectral: series.spectral,
            ref_year: info.t().saturating_sub(1),
            info,
            store: None,
            engine: None,
            view,
            scope: 0,
            comps: [0, 1, 2],
            stretch: 2.0,
            basis: None,
            basis_have: 0,
            basis_sent: 0,
            basis_local: false,
            ref_kind: 0,
            ref_pin: 0,
            ref_fixed_year: false,
            reference: Vec::new(),
            ref_key: 0,
            ref_text: String::new(),
            change_base: None,
            dir_mode: 0,
            dir_pin: 0,
            dir_ref_year: None,
            dir_auto: true,
            dir_threshold: 0.1,
            dir_basis: None,
            dir_have: 0,
            dir_sent: 0,
            dir_ref: Vec::new(),
            dir_ref_key: 0,
            dir_ref_text: String::new(),
            classes: Vec::new(),
            cls_method: 0,
            cls_lambda: 0.1,
            cls_min: 0.0,
            classifier: None,
            cls_error: String::new(),
            cls_gen: 1,
            cls_have: 0,
            cls_sent: 0,
            cls_from: None,
            range: [ViewRange::default(); 6],
            cmap: [viridis, viridis, viridis, plasma, rdbu, viridis],
            img: Vec::new(),
            slot_sent: HashMap::new(),
            years_seen: usize::MAX,
            results: Arc::new(Mutex::new(Results::default())),
        }))
    }

    pub fn drawn(&self) -> bool {
        self.view != View::Band
    }

    /// The engine has work running or waiting.
    pub fn busy(&self) -> bool {
        self.engine.as_ref().is_some_and(|e| e.busy())
    }

    /// The reference taken again (another kind, pin or year).
    pub fn reset_reference(&mut self) {
        self.ref_key = 0;
    }

    /// The year compared with year t (Change).
    fn base_of(&self, t: usize) -> Option<usize> {
        match self.change_base {
            None => t.checked_sub(1),
            Some(b) => Some(b),
        }
    }

    /// What the classes' image is drawn with: the colours and the minimum score.
    fn cls_style(&self) -> u64 {
        let mut h = mix(17, u64::from(self.cls_min.to_bits()));
        for c in &self.classes {
            h = mix(h, u64::from(u32::from_le_bytes(c.color.to_array())));
        }
        h
    }

    /// The threshold of change in use (None: the automatic one is not known yet).
    pub fn dir_thr(&self) -> Option<f32> {
        if self.dir_auto { self.dir_basis.as_ref().map(|b| b.threshold) } else { Some(self.dir_threshold) }
    }

    /// The classifier's scores: "probability" or "cosine".
    pub fn score_name(&self) -> &'static str {
        if self.cls_method == 1 { "cosine" } else { "probability" }
    }

    /// The view's range: set by hand or automatic, else the image's own.
    fn shown_range(&self, view: View, img: &Img) -> (f32, f32) {
        let r = &self.range[view as usize];
        if r.manual || r.key != 0 { (r.lo, r.hi) } else { (img.lo, img.hi) }
    }

    /// The image of year t into the map's frame, through the layer's placement.
    pub(crate) fn draw(
        &self,
        t: usize,
        place: &crate::layers::Placement,
        alpha: f32,
        gpu: &janus_gpu::Gpu,
        f: &mut janus_gpu::MapFrame,
        to_map: &dyn Fn([f64; 4]) -> [f32; 4],
    ) {
        let t = if self.spectral { 0 } else { t }; // the spectrum's one "year", whatever band the time bar shows
        let Some(img) = self.img.get(t) else { return };
        let Some(tex) = &img.tex else { return };
        let info = &self.info;
        let Some((q, warp)) = place.quad(info, 0.0, 0.0, info.width as f64, info.height as f64) else { return };
        if img.rgba {
            gpu.draw_image(f, tex, to_map(q), alpha, warp.as_ref(), true);
        } else {
            let view = self.view;
            let (lo, hi) = self.shown_range(view, img);
            gpu.draw_overlay(f, tex, to_map(q), [lo, hi], self.cmap[view as usize], alpha, warp.as_ref());
        }
    }
}

impl State {
    /// The date of layer i the embeddings show (a spectrum: its one "year").
    pub(crate) fn layer_date(&self, i: usize) -> usize {
        let l = &self.layers[i];
        if l.emb.as_ref().is_some_and(|e| e.spectral) {
            return 0;
        }
        let n = l.emb.as_ref().map_or(1, |e| e.info.t()).max(1);
        (if i == self.active { self.display.t } else { l.disp.t }).min(n - 1)
    }

    /// The layer whose embeddings the panel shows: the active one, else the top visible one, else any.
    pub(crate) fn embedding_layer(&self) -> Option<usize> {
        if self.layers.get(self.active).is_some_and(|l| l.emb.is_some()) {
            return Some(self.active);
        }
        let n = self.layers.len();
        (0..n)
            .rev()
            .find(|&i| self.layers[i].emb.is_some() && self.layers[i].visible)
            .or_else(|| (0..n).rev().find(|&i| self.layers[i].emb.is_some()))
    }

    /// The store pixel of layer i under the active layer's point (ax, ay); None off the layer.
    pub(crate) fn embedding_pixel(&self, i: usize, ax: f64, ay: f64) -> Option<usize> {
        let s = self.layers[i].emb.as_ref()?.store.as_ref()?;
        let (lx, ly) = if i == self.active { (ax, ay) } else { self.layers[i].place.to_layer(ax, ay)? };
        s.pixel_of(lx, ly)
    }

    /// The map's ROI on layer i, in store pixels (x0, y0, x1, y1), and a mask of a polygon's pixels in it.
    pub(crate) fn embedding_roi(&self, i: usize) -> Option<([usize; 4], Option<Vec<bool>>)> {
        let r = self.series.roi_rect?;
        let s = self.layers[i].emb.as_ref()?.store.as_ref()?;
        let poly = if i == self.active { self.series.roi_poly.clone() } else { self.roi_on_layer(&self.layers[i]) };
        let clamp = |v: f64, m: usize| v.clamp(0.0, m as f64) as usize;
        if !poly.is_empty() {
            // a polygon, or a rectangle in another CRS
            let mut p = poly;
            plane::transform(&mut p, |x, y| Some((x / s.fx, y / s.fy)));
            let b = plane::bounds(&p)?;
            let rect =
                [clamp(b[0].floor(), s.w), clamp(b[1].floor(), s.h), clamp(b[2].ceil(), s.w), clamp(b[3].ceil(), s.h)];
            if rect[2] <= rect[0] || rect[3] <= rect[1] {
                return None;
            }
            let (w, h) = (rect[2] - rect[0], rect[3] - rect[1]);
            let mask = plane::mask(&p, rect[0] as f64, rect[1] as f64, w as f64, h as f64, w, h);
            return Some((rect, Some(mask)));
        }
        let (mut x0, mut y0, mut x1, mut y1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
        for (ax, ay) in [(r[0], r[1]), (r[2], r[1]), (r[0], r[3]), (r[2], r[3])] {
            let (lx, ly) = if i == self.active {
                (ax as f64, ay as f64)
            } else {
                self.layers[i].place.to_layer(ax as f64, ay as f64)?
            };
            (x0, y0, x1, y1) = (x0.min(lx), y0.min(ly), x1.max(lx), y1.max(ly));
        }
        let rect = [
            clamp((x0 / s.fx).floor(), s.w),
            clamp((y0 / s.fy).floor(), s.h),
            clamp((x1 / s.fx).ceil(), s.w),
            clamp((y1 / s.fy).ceil(), s.h),
        ];
        (rect[2] > rect[0] && rect[3] > rect[1]).then_some((rect, None))
    }

    /// The similarity's reference vector: under the cursor, at a pin or the ROI's mean, in the year shown (or a
    /// fixed one).
    fn update_embedding_ref(&mut self, i: usize, t: usize) {
        let Some(e) = self.layers[i].emb.as_ref() else { return };
        let Some(s) = e.store.clone() else { return };
        let year = if e.ref_fixed_year { e.ref_year.min(s.t_n - 1) } else { t };
        if !s.year_ready(year) {
            return;
        }
        // "(2023)" after what the reference is; nothing for a spectrum's one "year"
        let when = if e.spectral { String::new() } else { format!(" ({})", e.info.layers[year].label) };
        let kind = e.ref_kind;
        let mut set: Option<(u64, Vec<f32>, String)> = None;
        let vector = |p: usize, text: String| -> Option<(u64, Vec<f32>, String)> {
            if !s.valid(year, p) {
                return None;
            }
            let key = mix(mix(mix(1, p as u64), year as u64), kind as u64);
            let mut v = vec![0f32; s.d];
            s.dequant(year, p, &mut v);
            Some((key, v, text))
        };
        match kind {
            0 => {
                if let Some(h) = &self.series.hover
                    && let Some(p) = self.embedding_pixel(i, h.x as f64 + 0.5, h.y as f64 + 0.5)
                {
                    set = vector(p, format!("the cursor{when}"));
                }
            }
            1 => {
                if let Some(pin) = self.series.pins.iter().find(|p| p.id == e.ref_pin)
                    && let Some(p) = self.embedding_pixel(i, pin.x as f64 + 0.5, pin.y as f64 + 0.5)
                {
                    set = vector(p, format!("pin {}{when}", pin.id));
                }
            }
            _ => {
                let Some((r, mask)) = self.embedding_roi(i) else { return };
                let mut key = mix(2, r[0] as u64);
                for v in [r[1], r[2], r[3], year] {
                    key = mix(key, v as u64);
                }
                key = mix(key, self.series.roi_gen);
                if key == e.ref_key {
                    return;
                }
                let (mut sum, mut v, mut n) = (vec![0f64; s.d], vec![0f32; s.d], 0usize);
                let step = (((r[2] - r[0]) as f64 * (r[3] - r[1]) as f64 / 200_000.0).sqrt() as usize).max(1);
                for y in (r[1]..r[3]).step_by(step) {
                    for x in (r[0]..r[2]).step_by(step) {
                        let p = y * s.w + x;
                        if mask.as_ref().is_some_and(|m| !m[(y - r[1]) * (r[2] - r[0]) + x - r[0]]) || !s.valid(year, p)
                        {
                            continue;
                        }
                        s.dequant(year, p, &mut v);
                        for (a, &b) in sum.iter_mut().zip(&v) {
                            *a += f64::from(b);
                        }
                        n += 1;
                    }
                }
                if n > 0 {
                    set = Some((
                        key,
                        sum.iter().map(|&x| (x / n as f64) as f32).collect(),
                        format!("the ROI mean{when}"),
                    ));
                }
            }
        }
        if let Some((key, v, text)) = set
            && key != e.ref_key
        {
            let e = self.layers[i].emb.as_mut().unwrap();
            (e.ref_key, e.reference, e.ref_text) = (key, v, text);
        }
    }

    /// The reference of "like a pin's change": the pin's difference vector between the year shown (or a fixed one)
    /// and the year compared with.
    fn update_direction_ref(&mut self, i: usize, t: usize) {
        let Some(e) = self.layers[i].emb.as_ref() else { return };
        let Some(s) = e.store.clone() else { return };
        let t = e.dir_ref_year.unwrap_or(t).min(s.t_n - 1);
        let Some(t0) = e.base_of(t).filter(|&b| b < s.t_n && b != t) else { return };
        let Some(pin) = self.series.pins.iter().find(|p| p.id == e.dir_pin) else { return };
        let Some(p) = self.embedding_pixel(i, pin.x as f64 + 0.5, pin.y as f64 + 0.5) else { return };
        let key = mix(mix(mix(3, p as u64), t as u64), t0 as u64);
        if key == e.dir_ref_key {
            return;
        }
        let Some(v) = change_vector(&s, t, t0, p) else { return };
        // how much the pin itself changed
        let (na, nb) = (f64::from(s.norm(t, p)), f64::from(s.norm(t0, p)));
        let dn: f64 = v.iter().map(|&x| f64::from(x) * f64::from(x)).sum();
        let dist = if na > 0.0 && nb > 0.0 { (dn - (na - nb).powi(2)) / (2.0 * na * nb) } else { 0.0 };
        let text = format!(
            "pin {} ({} - {}, cosine distance {:.3})",
            pin.id, e.info.layers[t].label, e.info.layers[t0].label, dist
        );
        let e = self.layers[i].emb.as_mut().unwrap();
        (e.dir_ref_key, e.dir_ref, e.dir_ref_text) = (key, v, text);
    }

    /// Samples for a class from layer i in the year shown: a pin's vector, or up to 200 of the ROI's; and where
    /// they came from.
    pub(crate) fn class_samples(&self, i: usize, from: ClassSource) -> Option<(Vec<Vec<f32>>, String)> {
        let e = self.layers[i].emb.as_ref()?;
        let s = e.store.as_ref()?;
        let t = self.layer_date(i);
        if !s.year_ready(t) {
            return None;
        }
        let when = if e.spectral { String::new() } else { format!(" ({})", e.info.layers[t].label) };
        let one = |p: usize| {
            let mut v = vec![0f32; s.d];
            s.dequant(t, p, &mut v);
            v
        };
        match from {
            ClassSource::Pin(id) => {
                let pin = self.series.pins.iter().find(|p| p.id == id)?;
                let p = self.embedding_pixel(i, pin.x as f64 + 0.5, pin.y as f64 + 0.5)?;
                s.valid(t, p).then(|| (vec![one(p)], format!("pin {id}{when}")))
            }
            ClassSource::Roi => {
                let (r, mask) = self.embedding_roi(i)?;
                let sample = sample_vectors(s, Some(r), 200, false, t, mask.as_deref());
                (!sample.is_empty()).then(|| {
                    let n = sample.len();
                    (sample.into_iter().map(|(_, p)| one(p as usize)).collect(), format!("{n} pixels of the ROI{when}"))
                })
            }
        }
    }

    /// --embedding-view: the view of the layer of embeddings, the panel open; "classes": a class from each pin once
    /// the year shown is read.
    fn scripted_embedding_view(&mut self) {
        let Some(v) = self.opts.embedding_view.clone() else { return };
        let Some(i) = self.embedding_layer() else { return };
        let first_pin = self.series.pins.first().map_or(0, |p| p.id);
        let e = self.layers[i].emb.as_mut().unwrap();
        e.view = match v.as_str() {
            "similarity" => View::Similarity,
            "change" => View::Change,
            "direction" | "direction-like" => View::Direction,
            "classes" => View::Classes,
            _ => View::Pca,
        };
        (e.ref_kind, e.ref_pin, e.dir_pin) = (1, first_pin, first_pin);
        e.dir_mode = usize::from(v == "direction-like");
        let n = e.info.t();
        self.show_embeddings = true;
        // a change needs a previous year: the last one shown
        if matches!(v.as_str(), "change" | "direction" | "direction-like") && i == self.active && n > 1 {
            self.set_date(n - 1);
        }
        self.map.dirty = true;
        if v == "classes" {
            let ids: Vec<(u32, Color32)> = self.series.pins.iter().map(|p| (p.id, p.color)).collect();
            let mut classes = Vec::new();
            for &(id, color) in &ids {
                let Some((samples, src)) = self.class_samples(i, ClassSource::Pin(id)) else { return };
                classes.push(EmbClass { name: format!("Pin {id}"), color, samples, sources: vec![src] });
            }
            let e = self.layers[i].emb.as_mut().unwrap();
            e.classes = classes;
            e.cls_gen += 1;
        }
        self.opts.embedding_view = None;
    }

    /// What the image of year t of layer i would be made from now (0: it cannot be made yet).
    pub(crate) fn embedding_key(&self, i: usize, t: usize) -> u64 {
        let Some(e) = self.layers[i].emb.as_ref() else { return 0 };
        let Some(s) = &e.store else { return 0 };
        if !s.year_ready(t) {
            return 0;
        }
        match e.view {
            View::Pca if e.basis.is_some() => mix(mix(11, e.basis_have), t as u64),
            View::Similarity if !e.reference.is_empty() => mix(mix(12, e.ref_key), t as u64),
            View::Change => match e.base_of(t) {
                Some(t0) if t0 < s.t_n && t0 != t && s.year_ready(t0) => mix(mix(13, t0 as u64), t as u64),
                _ => 0,
            },
            View::Direction => match e.base_of(t) {
                Some(t0) if t0 < s.t_n && t0 != t && s.year_ready(t0) => {
                    let pair = mix(t0 as u64, t as u64);
                    if e.dir_mode == 0 && e.dir_basis.is_some() {
                        mix(mix(14, e.dir_have), pair)
                    } else if e.dir_mode == 1
                        && !e.dir_ref.is_empty()
                        && let Some(thr) = e.dir_thr()
                    {
                        mix(mix(mix(16, e.dir_ref_key), u64::from(thr.to_bits())), pair)
                    } else {
                        0
                    }
                }
                _ => 0,
            },
            View::Classes if e.classifier.is_some() => mix(mix(mix(15, e.cls_have), e.cls_style()), t as u64),
            _ => 0,
        }
    }

    /// Once a frame: each layer's store and engine started when a view needs them, their results taken (a new basis,
    /// images to upload), a new basis asked for as years arrive or the ROI changes, the image of the date shown and
    /// of the next one while playing.
    pub(crate) fn pump_embeddings(&mut self, ctx: &egui::Context) {
        self.scripted_embedding_view();
        let settings = self.session_settings();
        let threads = settings.processing_threads();
        for i in 0..self.layers.len() {
            let Some(e) = self.layers[i].emb.as_mut() else { continue };
            if e.store.is_none() {
                if e.spectral && e.view == View::Band {
                    continue; // read once a view needs it
                }
                let c = ctx.clone();
                let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || c.request_repaint());
                e.store = Some(EmbeddingStore::start(
                    e.info.clone(),
                    settings.overview_budget_bytes as u64,
                    threads,
                    wake.clone(),
                ));
                e.engine = Some(EmbeddingEngine::new(threads, wake));
                e.img = (0..e.info.t()).map(|_| Img::default()).collect();
            }
            let s = e.store.clone().unwrap();
            // the results of the background work: a new basis, images to upload
            let (basis, done) = {
                let mut r = e.results.lock().unwrap();
                (r.basis.take(), std::mem::take(&mut r.done))
            };
            if let Some((b, key, local)) = basis {
                (e.basis, e.basis_have, e.basis_local) = (Some(b), key, local);
            }
            let (direction, classifier) = {
                let mut r = e.results.lock().unwrap();
                (r.direction.take(), r.classifier.take())
            };
            if let Some((b, key)) = direction {
                if e.dir_auto {
                    e.dir_threshold = b.threshold;
                }
                (e.dir_basis, e.dir_have) = (Some(b), key);
            }
            if let Some((c, key)) = classifier {
                e.cls_have = key;
                match c {
                    Ok(c) => (e.classifier, e.cls_error) = (Some(c), String::new()),
                    Err(why) => {
                        // no classes to draw: the images of the last classifier go
                        (e.classifier, e.cls_error) = (None, why);
                        for img in &mut e.img {
                            (img.tex, img.key) = (None, 0);
                        }
                        self.map.dirty = true;
                    }
                }
            }
            for d in done {
                let Some(img) = e.img.get_mut(d.t) else { continue };
                let Some(g) = self.gpu.as_ref() else {
                    e.slot_sent.clear(); // before the first frame: asked for again once there is a GPU
                    continue;
                };
                img.rgba = !d.rgba.is_empty();
                img.tex = Some(if img.rgba {
                    g.create_image(s.w as u32, s.h as u32, &d.rgba)
                } else {
                    g.create_tile(s.w as u32, s.h as u32, &d.values)
                });
                (img.key, img.ms, img.values, img.lo, img.hi) = (d.key, d.ms, d.values, d.lo, d.hi);
                (img.classes, img.counts) = (d.classes, d.counts);
                // the automatic range of a float view comes from the first image of each reference (or base), so
                // the years stay comparable
                let rk = match d.view {
                    View::Similarity => e.ref_key,
                    View::Direction => mix(e.dir_ref_key, u64::from(e.dir_threshold.to_bits())),
                    _ => e.change_base.map_or(6, |b| b as u64 + 7),
                };
                let r = &mut e.range[d.view as usize];
                if !img.rgba && d.view == e.view && !r.manual && r.key != rk {
                    (r.lo, r.hi, r.key) = (d.lo, d.hi, rk);
                }
                self.map.dirty = true;
            }
            if s.years_done() != e.years_seen {
                e.years_seen = s.years_done();
                self.map.dirty = true;
            }
            if e.view == View::Band || !s.ranges_ready() || s.years_done() == 0 {
                continue;
            }
            let t = self.layer_date(i);
            // principal components: of every year (fitted again as each year arrives), or of the ROI (local PCA)
            if self.layers[i].emb.as_ref().unwrap().view == View::Pca {
                let scope = self.layers[i].emb.as_ref().unwrap().scope;
                let roi = if scope == 1 { self.embedding_roi(i) } else { None };
                let e = self.layers[i].emb.as_mut().unwrap();
                let local = roi.is_some();
                let mut want = mix(
                    mix(mix(21, u64::from(local)), s.years_done() as u64),
                    (e.comps[0] + 8 * e.comps[1] + 64 * e.comps[2]) as u64,
                );
                want = mix(want, u64::from(e.stretch.to_bits()));
                if let Some((r, mask)) = &roi {
                    for v in r {
                        want = mix(want, *v as u64);
                    }
                    if mask.is_some() {
                        want = mix(want, self.series.roi_gen); // the same box, another polygon
                    }
                }
                if want != e.basis_have && want != e.basis_sent {
                    e.basis_sent = want;
                    let (store, results, comps, stretch) = (s.clone(), e.results.clone(), e.comps, e.stretch);
                    e.engine.as_ref().unwrap().submit("basis", move |pool| {
                        let (rect, mask) = roi.map_or((None, None), |(r, m)| (Some(r), m));
                        let sample = sample_vectors(&store, rect, 65536, true, 0, mask.as_deref());
                        let mut b = fit_pca(&store, &sample, 6, comps, stretch, pool);
                        b.years = store.years_done();
                        results.lock().unwrap().basis = Some((Arc::new(b), want, local));
                    });
                }
            }
            if self.layers[i].emb.as_ref().unwrap().view == View::Similarity {
                self.update_embedding_ref(i, t);
            }
            // the direction of change: its principal directions over every pair of years (one colour for one kind
            // of change in all of them), or the reference pin's change
            if self.layers[i].emb.as_ref().unwrap().view == View::Direction {
                if self.layers[i].emb.as_ref().unwrap().dir_mode == 1 {
                    self.update_direction_ref(i, t);
                }
                // fitted in both modes: the automatic threshold comes with it
                let e = self.layers[i].emb.as_mut().unwrap();
                let pairs: Vec<(usize, usize)> =
                    (0..s.t_n).filter_map(|y| e.base_of(y).filter(|&b| b < s.t_n && b != y).map(|b| (y, b))).collect();
                let thr = if e.dir_auto { -1.0 } else { e.dir_threshold };
                let mut want = mix(mix(31, s.years_done() as u64), u64::from(thr.to_bits()));
                want = mix(mix(want, u64::from(e.stretch.to_bits())), e.change_base.map_or(0, |b| b as u64 + 1));
                if want != e.dir_have && want != e.dir_sent {
                    e.dir_sent = want;
                    let (store, results, stretch) = (s.clone(), e.results.clone(), e.stretch);
                    e.engine.as_ref().unwrap().submit("direction", move |pool| {
                        let b = fit_direction(&store, &pairs, thr, stretch, pool);
                        results.lock().unwrap().direction = Some((Arc::new(b), want));
                    });
                }
            }
            // the classifier, fitted again when the classes or the method change
            if self.layers[i].emb.as_ref().unwrap().view == View::Classes {
                let e = self.layers[i].emb.as_mut().unwrap();
                let want = mix(mix(41, e.cls_gen), s.years_done() as u64);
                if want != e.cls_have && want != e.cls_sent {
                    e.cls_sent = want;
                    let train: Vec<Vec<Vec<f32>>> = e.classes.iter().map(|c| c.samples.clone()).collect();
                    let method = if e.cls_method == 1 { Method::Centroid } else { Method::Logistic };
                    let (store, results, lambda) = (s.clone(), e.results.clone(), f64::from(e.cls_lambda));
                    e.engine.as_ref().unwrap().submit("classifier", move |_| {
                        let c = if train.len() < 2 {
                            Err("add two classes or more".to_string())
                        } else {
                            let norm = layer_norm(&store);
                            fit(&train, method, lambda, norm.as_ref().map(|(m, sd)| (m.as_slice(), sd.as_slice())))
                                .map(Arc::new)
                        };
                        results.lock().unwrap().classifier = Some((c, want));
                    });
                }
            }
            // the image of the date shown, of the next one while playing, and of the year the classes' transitions
            // are counted from
            let n = s.t_n;
            let next = (self.display.playing && i == self.active).then_some((t + 1) % n);
            let from = {
                let e = self.layers[i].emb.as_ref().unwrap();
                e.cls_from.filter(|&y| e.view == View::Classes && y < n && y != t)
            };
            for (slot, tt) in [("image", Some(t)), ("image+1", next), ("image-from", from)] {
                let Some(tt) = tt else { continue };
                let key = self.embedding_key(i, tt);
                let e = self.layers[i].emb.as_mut().unwrap();
                if key == 0 || e.img[tt].key == key || e.slot_sent.get(slot) == Some(&key) {
                    continue;
                }
                e.slot_sent.insert(slot, key);
                let (store, results, view, basis, reference) =
                    (s.clone(), e.results.clone(), e.view, e.basis.clone(), e.reference.clone());
                let (dir_mode, dir_basis, dir_ref, dir_thr) =
                    (e.dir_mode, e.dir_basis.clone(), e.dir_ref.clone(), e.dir_thr().unwrap_or(0.0));
                let (classifier, cls_min) = (e.classifier.clone(), e.cls_min);
                let colours: Vec<Color32> = e.classes.iter().map(|c| c.color).collect();
                let t0 = e.base_of(tt);
                e.engine.as_ref().unwrap().submit(slot, move |pool| {
                    let c0 = std::time::Instant::now();
                    let mut d = Done {
                        t: tt,
                        view,
                        key,
                        rgba: Vec::new(),
                        values: Vec::new(),
                        classes: Vec::new(),
                        counts: Vec::new(),
                        lo: 0.0,
                        hi: 1.0,
                        ms: 0.0,
                    };
                    let t0 = t0.unwrap_or(0);
                    match view {
                        View::Pca => {
                            if let Some(b) = &basis {
                                d.rgba = render_pca_rgb(&store, tt, b, pool);
                            }
                        }
                        View::Direction if dir_mode == 0 => {
                            if let Some(b) = &dir_basis {
                                d.rgba = render_direction(&store, tt, t0, b, pool);
                            }
                        }
                        View::Classes => {
                            if let Some(c) = &classifier {
                                let (cl, sc) = classify_year(&store, tt, c, cls_min, pool);
                                // each class in its colour, below the minimum score grey, no vector transparent
                                let mut rgba = vec![0u8; cl.len() * 4];
                                let mut counts = vec![0usize; c.k + 1];
                                for (p, (&k, &v)) in cl.iter().zip(&sc).enumerate() {
                                    if v.is_nan() {
                                        continue;
                                    }
                                    let col = if k == NONE {
                                        counts[c.k] += 1;
                                        Color32::from_gray(48)
                                    } else {
                                        counts[usize::from(k)] += 1;
                                        colours.get(usize::from(k)).copied().unwrap_or(Color32::WHITE)
                                    };
                                    rgba[p * 4..p * 4 + 4].copy_from_slice(&col.to_array());
                                }
                                (d.rgba, d.values, d.classes, d.counts) = (rgba, sc, cl, counts);
                            }
                        }
                        _ => {
                            d.values = match view {
                                View::Similarity => render_similarity(&store, tt, &reference, pool),
                                View::Direction => render_change_like(&store, tt, t0, &dir_ref, dir_thr, pool),
                                _ => render_change(&store, tt, t0, pool),
                            };
                            let hi = if view == View::Change { 98.0 } else { 99.5 };
                            (d.lo, d.hi) = percentile_range(&d.values, 2.0, hi);
                        }
                    }
                    d.ms = c0.elapsed().as_secs_f64() * 1000.0;
                    results.lock().unwrap().done.push(d);
                });
            }
        }
    }

    /// The main map's title, colour bar (colormap, ends) and wait message when the active layer is drawn as
    /// embeddings; None when it is not.
    pub(crate) fn embedding_map_info(&self) -> Option<MapInfo> {
        let l = self.layers.get(self.active)?;
        let e = l.emb.as_ref().filter(|e| e.drawn())?;
        let info = &e.info;
        let t = self.layer_date(self.active);
        let what = if e.spectral { "Spectrum" } else { "Embeddings" };
        let head = if e.spectral { l.name.clone() } else { info.layers[t].label.clone() }; // a spectrum has no date
        let mut bar = None;
        let title = match e.view {
            View::Pca => format!(
                "{head}  |  {what}: PC{}, PC{}, PC{}{}",
                e.comps[0] + 1,
                e.comps[1] + 1,
                e.comps[2] + 1,
                if e.basis_local { " of the ROI" } else { "" }
            ),
            View::Similarity => {
                bar = Some(View::Similarity);
                format!(
                    "{head}  |  {} similarity to {}",
                    if e.spectral { "Spectral (cosine)" } else { "Cosine" },
                    if e.ref_text.is_empty() { "(no reference)" } else { &e.ref_text }
                )
            }
            View::Classes => {
                let names: Vec<&str> = e.classes.iter().map(|c| c.name.as_str()).collect();
                let how = if e.cls_method == 1 { "nearest centroid" } else { "logistic regression" };
                format!("{head}  |  Classes ({how}): {}", names.join(", "))
            }
            _ => {
                let base = e
                    .base_of(t)
                    .filter(|&b| b < info.t())
                    .map_or("(none)".to_string(), |b| info.layers[b].label.clone());
                let pair = format!("{} - {base}", info.layers[t].label);
                if e.view == View::Direction && e.dir_mode == 0 {
                    format!("{pair}  |  Direction of change: PC1-3 of the changes above {:.3}", e.dir_threshold)
                } else if e.view == View::Direction {
                    bar = Some(View::Direction);
                    let r = if e.dir_ref_text.is_empty() { "(no pin)" } else { &e.dir_ref_text };
                    format!("{pair}  |  Change like {r}")
                } else {
                    bar = Some(View::Change);
                    format!("{pair}  |  Change (cosine distance)")
                }
            }
        };
        let bar = bar.and_then(|v| {
            let img = e.img.get(t)?;
            let (lo, hi) = e.shown_range(v, img);
            Some((e.cmap[v as usize], lo, hi))
        });
        let s = e.store.as_ref();
        let wait = match s {
            Some(s) if s.failed() => format!("{what}: {}", s.error()),
            Some(s) if !s.ranges_ready() || !s.year_ready(t) => {
                if e.spectral {
                    "Reading the spectrum...".into()
                } else if s.ranges_ready() {
                    "Reading this year...".into()
                } else {
                    "Reading the embeddings...".into()
                }
            }
            None => "Reading the embeddings...".into(),
            _ if e.view == View::Similarity && e.reference.is_empty() => match e.ref_kind {
                0 => "Hover over the map: the similarity to the vector under the cursor".into(),
                1 => "Drop the pin chosen as reference (Embeddings panel)".into(),
                _ => "Draw an ROI (Shift + drag): the similarity to its mean vector".into(),
            },
            _ if matches!(e.view, View::Change | View::Direction) && e.base_of(t).is_none() => {
                "The first year has no previous year".into()
            }
            _ if e.view == View::Direction
                && e.dir_mode == 0
                && e.dir_basis.as_ref().is_some_and(|b| b.samples < 3) =>
            {
                format!("No pixel changed by more than {:.3} (cosine distance): lower the threshold", e.dir_threshold)
            }
            _ if e.view == View::Direction && e.dir_mode == 1 && e.dir_ref.is_empty() => {
                "Drop a pin on a change and pick it (Embeddings panel)".into()
            }
            _ if e.view == View::Classes && e.classifier.is_none() && e.classes.len() < 2 => {
                "Add two classes or more: drop pins on known places, then add them (Embeddings panel)".into()
            }
            _ if e.view == View::Classes && e.classifier.is_none() && !e.cls_error.is_empty() => {
                format!("Classes: {}", e.cls_error)
            }
            _ if e.view == View::Pca && e.basis.as_ref().is_some_and(|b| b.samples < 2) => {
                format!(
                    "No data {}: no vectors to fit the principal components",
                    if e.basis_local { "in the ROI" } else { "in this layer" }
                )
            }
            _ if e.img.get(t).is_none_or(|i| i.tex.is_none()) => "Computing...".into(),
            _ => String::new(),
        };
        Some((title, bar, wait))
    }

    /// The embedding view's value under the cursor, for the status line ("" = none).
    pub(crate) fn embedding_status_at(&self, ix: usize, iy: usize) -> String {
        let Some(e) = self.layers.get(self.active).and_then(|l| l.emb.as_ref()).filter(|e| e.drawn()) else {
            return String::new();
        };
        let Some(s) = &e.store else { return String::new() };
        let t = self.layer_date(self.active);
        let Some(p) = self.embedding_pixel(self.active, ix as f64 + 0.5, iy as f64 + 0.5).filter(|_| s.year_ready(t))
        else {
            return String::new();
        };
        if !s.valid(t, p) {
            return if e.spectral { "  |  no spectrum".into() } else { "  |  no embedding".into() };
        }
        let g = |v: f64| janus_cube::multidim::format_g(v, 3);
        if e.view == View::Pca
            && let Some(b) = &e.basis
        {
            let mut v = vec![0f32; s.d];
            s.dequant(t, p, &mut v);
            let pc: Vec<String> = (0..3)
                .map(|c| {
                    let x: f32 = (0..s.d).map(|k| (v[k] - b.mean[k]) * b.comps[b.shown[c] * s.d + k]).sum();
                    format!("PC{} {}", e.comps[c] + 1, g(f64::from(x)))
                })
                .collect();
            return format!("  |  {}", pc.join("  "));
        }
        if e.view == View::Classes {
            let img = e.img.get(t);
            let (Some(&k), Some(&v)) = (img.and_then(|i| i.classes.get(p)), img.and_then(|i| i.values.get(p))) else {
                return String::new();
            };
            let name = e.classes.get(usize::from(k)).map_or("no class (below the minimum)", |c| c.name.as_str());
            return format!("  |  {name}, {} {v:.3}", e.score_name());
        }
        if e.view == View::Direction
            && e.dir_mode == 0
            && let (Some(b), Some(t0)) = (&e.dir_basis, e.base_of(t))
            && b.comps.len() == 3 * s.d
            && let Some(dv) = change_vector(s, t, t0, p)
        {
            let pc: Vec<String> = (0..3)
                .map(|c| {
                    let x: f32 = (0..s.d).map(|k| (dv[k] - b.mean[k]) * b.comps[c * s.d + k]).sum();
                    format!("PC{} {}", c + 1, g(f64::from(x)))
                })
                .collect();
            return format!("  |  change {}", pc.join("  "));
        }
        let what = match e.view {
            View::Similarity => "similarity",
            View::Direction => "like the pin's change",
            _ => "change",
        };
        match e.img.get(t).and_then(|i| i.values.get(p)).filter(|v| !v.is_nan()) {
            Some(v) => format!("  |  {what} {v:.4}"),
            None => String::new(),
        }
    }

    /// The embeddings being read: their progress top right of the map (under the overview's bar).
    pub(crate) fn draw_embedding_notes(&self, painter: &egui::Painter, rect: Rect) {
        let mut y = 10.0 + if self.session.as_ref().is_some_and(|s| !s.s.overview.complete()) { 24.0 } else { 0.0 };
        let bw = 300.0;
        let font = body_font(painter);
        for l in self.layers.iter().filter(|l| l.visible) {
            let Some(s) = l.emb.as_ref().and_then(|e| e.store.as_ref()) else { continue };
            let (frac, text, col) = if s.failed() {
                (1.0, format!("Embeddings: {}", s.error()), Color32::from_rgba_unmultiplied(170, 50, 50, 220))
            } else if !s.complete() && s.t_n > 0 {
                (
                    s.years_done() as f32 / s.t_n as f32,
                    format!("Reading embeddings {}/{} years", s.years_done(), s.t_n),
                    Color32::from_rgba_unmultiplied(70, 130, 220, 220),
                )
            } else {
                continue;
            };
            let p0 = Pos2::new(rect.right() - bw - 10.0, rect.top() + y);
            painter.rect_filled(Rect::from_min_size(p0, vec2(bw, 18.0)), 3.0, Color32::from_black_alpha(170));
            painter.rect_filled(Rect::from_min_size(p0, vec2(bw * frac.clamp(0.0, 1.0), 18.0)), 3.0, col);
            painter.with_clip_rect(Rect::from_min_size(p0, vec2(bw, 18.0))).text(
                p0 + vec2(6.0, 2.0),
                egui::Align2::LEFT_TOP,
                text,
                font.clone(),
                Color32::WHITE,
            );
            y += 24.0;
        }
        self.draw_download_notes(painter, rect, y);
    }

    /// Every layer drawn as embeddings has the image of its date (or failed to read).
    pub(crate) fn state_embeddings_settled(&self) -> bool {
        (0..self.layers.len()).all(|i| {
            let Some(e) = self.layers[i].emb.as_ref().filter(|e| e.drawn()) else { return true };
            if e.store.as_ref().is_some_and(|s| s.failed()) {
                return true;
            }
            // a basis or classifier fitted again (the last years read) may still be on its way
            if e.busy() || e.store.as_ref().is_some_and(|s| !s.complete()) {
                return false;
            }
            let t = self.layer_date(i);
            // a change with no year to compare with: nothing to wait for
            if matches!(e.view, View::Change | View::Direction) && e.base_of(t).is_none() {
                return true;
            }
            let key = self.embedding_key(i, t);
            key != 0 && e.img.get(t).is_some_and(|img| img.has_texture() && img.key == key)
        })
    }

    /// A spectrum's analysis: the panel, and its principal components on the map.
    pub fn show_spectral_analysis(&mut self) {
        self.show_embeddings = true;
        if let Some(e) = self.layers.get_mut(self.active).and_then(|l| l.emb.as_mut())
            && e.spectral
            && e.view == View::Band
        {
            e.view = View::Pca;
            self.map.dirty = true;
        }
    }

    /// The view of the panel's layer as a GeoTIFF at full resolution: the whole layer or the visible area.
    pub(crate) fn embedding_export_spec(&self, whole_layer: bool) -> Result<EmbeddingExport, String> {
        let i = self.embedding_layer().filter(|&i| self.layers[i].emb.as_ref().is_some_and(|e| e.drawn()));
        let i = i.ok_or("No layer is drawn as embeddings (Embeddings panel).")?;
        let (l, e) = (&self.layers[i], self.layers[i].emb.as_ref().unwrap());
        let info = e.info.clone();
        let t = self.layer_date(i);
        let win = if whole_layer {
            [0, 0, info.width, info.height]
        } else {
            self.embedding_visible_window(i).ok_or("The layer of embeddings is not in view.")?
        };
        let year = info.layers[t].label.clone();
        let size = format!("{} x {} px", win[2] - win[0], win[3] - win[1]);
        let model = if e.spectral { format!("spectrum of {} bands", info.bands_per_date) } else { e.meta.label() };
        let mut x = EmbeddingExport {
            info: info.clone(),
            kind: ExportKind::Pca,
            t,
            t0: None,
            basis: None,
            reference: Vec::new(),
            direction: None,
            classifier: None,
            threshold: 0.0,
            skip: e.store.as_ref().and_then(|s| s.ranges()).map(|r| r.skip.clone()).unwrap_or_default(),
            win,
            title: String::new(),
            band_names: Vec::new(),
            metadata: vec![("JANUS_EMBEDDING_MODEL".into(), model), ("JANUS_DATE".into(), year.clone())],
        };
        if !e.meta.attribution.is_empty() {
            x.metadata.push(("JANUS_EMBEDDING_ATTRIBUTION".into(), e.meta.attribution.clone()));
        }
        if !e.meta.license.is_empty() {
            x.metadata.push(("JANUS_EMBEDDING_LICENSE".into(), e.meta.license.clone()));
        }
        match e.view {
            View::Pca => {
                let b = e.basis.clone().ok_or("The principal components are still being fitted.")?;
                x.band_names = (0..b.k)
                    .map(|c| {
                        format!(
                            "PC{} ({:.1}% of the variance)",
                            c + 1,
                            if b.total > 0.0 { 100.0 * b.eig[c] / b.total } else { 0.0 }
                        )
                    })
                    .collect();
                let what = format!("principal components{}", if e.basis_local { " of the ROI" } else { "" });
                x.metadata.push(("JANUS_EMBEDDING_VIEW".into(), what.clone()));
                x.metadata.push(("JANUS_PCA_SAMPLES".into(), format!("{} vectors of {} years", b.samples, b.years)));
                x.title = format!("{}: {what} {year}, {size} ({} bands, Float32)", l.name, b.k);
                x.basis = Some(b);
            }
            View::Similarity => {
                if e.reference.is_empty() {
                    return Err(
                        "No reference vector yet: hover over the map, drop the reference pin or draw an ROI.".into()
                    );
                }
                x.kind = ExportKind::Similarity;
                x.reference = e.reference.clone();
                x.band_names = vec![format!("cosine similarity to {}", e.ref_text)];
                x.metadata.push(("JANUS_EMBEDDING_VIEW".into(), x.band_names[0].clone()));
                x.title = format!("{}: similarity {year}, {size} (Float32)", l.name);
            }
            View::Classes => {
                let c = e.classifier.clone().ok_or("No classifier yet: add two classes or more.")?;
                x.kind = ExportKind::Classes;
                x.threshold = e.cls_min;
                let names: Vec<String> =
                    e.classes.iter().enumerate().map(|(k, c)| format!("{} {}", k + 1, c.name)).collect();
                x.band_names = vec![format!("class ({})", names.join(", ")), e.score_name().into()];
                let how = if e.cls_method == 1 { "nearest centroid (cosine)" } else { "logistic regression (L2)" };
                x.metadata.push(("JANUS_EMBEDDING_VIEW".into(), format!("classes, {how}")));
                x.metadata.push(("JANUS_CLASSES".into(), names.join(";")));
                let colours: Vec<String> = e
                    .classes
                    .iter()
                    .map(|c| format!("#{:02x}{:02x}{:02x}", c.color.r(), c.color.g(), c.color.b()))
                    .collect();
                x.metadata.push(("JANUS_CLASS_COLOURS".into(), colours.join(";")));
                x.metadata.push(("JANUS_MIN_SCORE".into(), e.cls_min.to_string()));
                x.title = format!("{}: classes {year}, {size} (2 bands, Float32)", l.name);
                x.classifier = Some(c);
            }
            View::Direction => {
                let t0 = e.base_of(t).filter(|&b| b < info.t() && b != t).ok_or("No year to compare this one with.")?;
                let pair = format!("{year} - {}", info.layers[t0].label);
                let thr = e.dir_thr().ok_or("The threshold of change is still being worked out.")?;
                (x.t0, x.threshold) = (Some(t0), thr);
                x.metadata.push(("JANUS_CHANGE_THRESHOLD".into(), thr.to_string()));
                if e.dir_mode == 0 {
                    let b = e.dir_basis.clone().ok_or("The directions of change are still being fitted.")?;
                    x.kind = ExportKind::Direction;
                    x.band_names = (0..3)
                        .map(|c| {
                            let pct = if b.total > 0.0 { 100.0 * b.eig[c] / b.total } else { 0.0 };
                            format!("PC{} of the change {pair} ({pct:.1}% of its variance)", c + 1)
                        })
                        .collect();
                    x.metadata.push(("JANUS_EMBEDDING_VIEW".into(), "principal directions of change".into()));
                    x.title = format!("{}: direction of change {pair}, {size} (3 bands, Float32)", l.name);
                    x.direction = Some(b);
                } else {
                    if e.dir_ref.is_empty() {
                        return Err("No reference change yet: pick a pin on a change.".into());
                    }
                    x.kind = ExportKind::ChangeLike;
                    x.reference = e.dir_ref.clone();
                    x.band_names = vec![format!("cosine of the change {pair} to that of {}", e.dir_ref_text)];
                    x.metadata.push(("JANUS_EMBEDDING_VIEW".into(), x.band_names[0].clone()));
                    x.title = format!("{}: change like a pin's {pair}, {size} (Float32)", l.name);
                }
            }
            _ => {
                let t0 = e.base_of(t).filter(|&b| b < info.t() && b != t).ok_or("No year to compare this one with.")?;
                x.kind = ExportKind::Change;
                x.t0 = Some(t0);
                x.band_names = vec![format!("cosine distance {year} - {}", info.layers[t0].label)];
                x.metadata.push(("JANUS_EMBEDDING_VIEW".into(), x.band_names[0].clone()));
                x.title = format!("{}: change {year} - {}, {size} (Float32)", l.name, info.layers[t0].label);
            }
        }
        Ok(x)
    }

    /// The map's visible area on layer i: the bounding box of points along the canvas' edges (curved when the layer
    /// is in another CRS).
    fn embedding_visible_window(&self, i: usize) -> Option<[usize; 4]> {
        let g = self.grid()?;
        let info = &self.layers[i].emb.as_ref()?.info;
        let m = &self.map;
        let [cw, ch] = m.canvas.map(f64::from);
        let (mut x0, mut y0, mut x1, mut y1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
        let n = 16;
        for k in 0..=n {
            let f = k as f64 / n as f64;
            for (cx, cy) in [(f * cw, 0.0), (f * cw, ch), (0.0, f * ch), (cw, f * ch)] {
                // the canvas point -> the active layer's pixel
                let ax = (m.center[0] + (cx - cw / 2.0) / m.scale) * g.fx;
                let ay = (m.center[1] + (cy - ch / 2.0) / m.scale) * g.fy;
                let Some((lx, ly)) =
                    (if i == self.active { Some((ax, ay)) } else { self.layers[i].place.to_layer(ax, ay) })
                else {
                    continue;
                };
                (x0, y0, x1, y1) = (x0.min(lx), y0.min(ly), x1.max(lx), y1.max(ly));
            }
        }
        let c = |v: f64, m: usize| v.clamp(0.0, m as f64) as usize;
        let w = [
            c(x0.floor(), info.width),
            c(y0.floor(), info.height),
            c(x1.ceil(), info.width),
            c(y1.ceil(), info.height),
        ];
        (x1 >= x0 && w[2] > w[0] && w[3] > w[1]).then_some(w)
    }

    /// The view written as a Float32 GeoTIFF on the layer's grid (tiled, DEFLATE), a band per value, in the
    /// background.
    pub fn export_embeddings(&mut self, path: String, spec: EmbeddingExport, ctx: &egui::Context) -> Arc<ExportJob> {
        let threads = self.session_settings().processing_threads();
        let title = spec.title.clone();
        let job = start_export_job(title, path.clone(), ctx, move |j| write_embedding_tiff(&path, &spec, threads, j));
        self.exports.push(job.clone());
        self.show_exports = true;
        job
    }

    /// The Embeddings panel: the layer, its view and the view's options, the charts of the cursor and the pins.
    pub(crate) fn embeddings_ui(&mut self, ui: &mut Ui) {
        let Some(i) = self.embedding_layer() else {
            ui.label("No layer of embeddings. Open one (a folder of yearly files from Zeit or Janus) or add the embeddings of the visible area:");
            self.embedding_download_ui(ui);
            return;
        };
        let t = self.layer_date(i);
        let pins: Vec<(u32, Color32)> = self.series.pins.iter().map(|p| (p.id, p.color)).collect();
        let hover = self.series.hover.as_ref().map(|h| (h.x, h.y));
        let pin_px: Vec<(u32, usize, usize, Color32)> =
            self.series.pins.iter().map(|p| (p.id, p.x, p.y, p.color)).collect();
        let roi = self.embedding_roi(i).is_some();
        let active = self.active;
        let series_range =
            self.layers[i].session.as_ref().or(if i == active { self.session.as_ref() } else { None }).map(|s| {
                let n = s.info.t();
                (s.info.layers[0].time, s.info.layers[n - 1].time)
            });
        let probes: Vec<(String, usize, Color32)> = {
            let mut v = Vec::new();
            if let Some((x, y)) = hover
                && let Some(p) = self.embedding_pixel(i, x as f64 + 0.5, y as f64 + 0.5)
            {
                v.push(("cursor".to_string(), p, theme::cursor_series()));
            }
            for &(id, x, y, c) in &pin_px {
                if let Some(p) = self.embedding_pixel(i, x as f64 + 0.5, y as f64 + 0.5) {
                    v.push((format!("pin {id}"), p, c));
                }
            }
            v
        };
        let name = self.layers[i].name.clone();
        let mut dirty = false;
        let mut export: Option<bool> = None;
        // samples to add: to class k (None: a new class), from a pin or the ROI
        let mut add_samples: Option<(Option<usize>, ClassSource)> = None;
        let e = self.layers[i].emb.as_mut().unwrap();
        let info = e.info.clone();
        let t_n = info.t();
        ui.colored_label(theme::accent(), &name);
        let head = if e.spectral {
            let (a, b) = series_range.unwrap_or_default();
            ui.label(format!(
                "Spectrum of {} bands, {a:.0} to {b:.0} nm: each pixel's bands as one vector",
                info.bands_per_date
            ))
        } else {
            ui.label(format!("{}, {} to {}", e.meta.label(), info.layers[0].label, info.layers[t_n - 1].label))
        };
        if !e.meta.attribution.is_empty() || !e.meta.license.is_empty() {
            head.on_hover_text(format!("{}\nLicence: {}", e.meta.attribution, e.meta.license));
        }
        if i != active {
            ui.weak("(not the active layer: drawn on top of it)");
        }
        if let Some(s) = &e.store {
            if s.failed() {
                ui.colored_label(theme::error(), s.error());
            } else if !s.complete() && e.spectral {
                ui.weak(format!("reading the spectrum ({} x {} px)", s.w, s.h));
            } else if !s.complete() {
                ui.weak(format!("reading {}/{} years ({} x {} px)", s.years_done(), s.t_n, s.w, s.h));
            } else {
                ui.weak(format!(
                    "{} x {} px (1:{}), {:.0} MB, read in {:.1} s",
                    s.w,
                    s.h,
                    janus_cube::multidim::format_g(s.fx, 2),
                    s.bytes() as f64 / 1048576.0,
                    s.seconds()
                ));
            }
        }
        crate::panels::section(ui, "View");
        for v in [View::Pca, View::Similarity, View::Change, View::Direction, View::Classes] {
            // a spectrum has one "year": no change
            if e.spectral && matches!(v, View::Change | View::Direction) {
                continue;
            }
            let tip = match v {
                View::Direction => {
                    "The kinds of change apart: the principal components of the changes'\n\
                                    difference vectors as RGB, or how much each change is like a pin's"
                }
                View::Classes => {
                    "A class map from a few examples: pins (or ROI pixels) of each class,\n\
                                  a classifier fitted at once and applied to every year"
                }
                _ => "",
            };
            let r = ui.radio_value(&mut e.view, v, VIEW_NAMES[v as usize]);
            let r = if tip.is_empty() { r } else { r.on_hover_text(tip) };
            if r.changed() {
                dirty = true;
            }
        }
        let band = if e.spectral { "Bands (Display panel)" } else { VIEW_NAMES[0] };
        if ui
            .radio_value(&mut e.view, View::Band, band)
            .on_hover_text(if e.spectral {
                "The spectrum's bands and composites, as without this panel"
            } else {
                "One dimension as a band, with every mode of the Display panel"
            })
            .changed()
        {
            dirty = true;
        }
        match e.view {
            View::Pca => {
                crate::panels::section(ui, "Principal components");
                ui.horizontal(|ui| {
                    let tip = if e.spectral {
                        "PCA of the whole scene's spectra (a sample of 65536), the first three as RGB:\n\
                         the main ways the spectra differ, whatever bands they lie in."
                    } else {
                        "One PCA for every year: a colour means the same thing in each year,\n\
                         so playing the series shows real change."
                    };
                    dirty |= ui
                        .radio_value(&mut e.scope, 0, if e.spectral { "Every pixel" } else { "Every pixel and year" })
                        .on_hover_text(tip)
                        .changed();
                    dirty |= ui
                        .radio_value(&mut e.scope, 1, "ROI (local)")
                        .on_hover_text(
                            "PCA of the ROI's pixels only (every year), applied to the whole map: the\n\
                             colours spread over the differences inside the ROI (outside it, they saturate).\n\
                             Draw the ROI again (Shift + drag, or R for a polygon): it is fitted again at once.",
                        )
                        .changed();
                });
                if e.scope == 1 && !roi {
                    ui.colored_label(theme::warning(), "Draw an ROI: Shift + drag on the map, or R for a polygon");
                }
                let k = e.basis.as_ref().map_or(6, |b| b.k);
                ui.horizontal(|ui| {
                    for (c, n) in ["R", "G", "B"].iter().enumerate() {
                        let mut v = e.comps[c] + 1;
                        let w = ui.available_width() / (3 - c) as f32 - 16.0;
                        ui.label(*n);
                        if ui
                            .add_sized(
                                [w.max(30.0), ui.spacing().interact_size.y],
                                egui::DragValue::new(&mut v).range(1..=k).prefix("PC"),
                            )
                            .changed()
                        {
                            e.comps[c] = v - 1;
                            dirty = true;
                        }
                    }
                });
                ui.add(egui::Slider::new(&mut e.stretch, 0.0..=10.0).text("% clipped (stretch)"))
                    .on_hover_text("Each component is stretched between these percentiles of the sample");
                if let Some(b) = &e.basis {
                    let ev: Vec<String> = (0..b.k)
                        .map(|c| {
                            format!("PC{} {:.1}%", c + 1, if b.total > 0.0 { 100.0 * b.eig[c] / b.total } else { 0.0 })
                        })
                        .collect();
                    ui.label(ev.join("  ")).on_hover_text("Share of the variance along each component");
                    let img_ms = e.img.get(t).map_or(0.0, |i| i.ms);
                    ui.weak(format!("{} vectors, {:.1} ms ({})", b.samples, b.ms, janus_embed::simd::name()))
                        .on_hover_text(format!(
                            "Covariance {:.2} ms, eigenvectors {:.2} ms ({} iterations of subspace iteration);\n\
                         image of a year: {img_ms:.1} ms",
                            b.cov_ms, b.eig_ms, b.iterations
                        ));
                } else {
                    ui.weak("fitting...");
                }
            }
            View::Similarity => {
                crate::panels::section(ui, "Reference");
                let kinds = ["Cursor (live)", "Pin", "ROI mean"];
                let before = e.ref_kind;
                egui::ComboBox::from_id_salt("emb ref kind")
                    .width(ui.available_width())
                    .selected_text(kinds[e.ref_kind])
                    .show_ui(ui, |ui| {
                        for (k, n) in kinds.iter().enumerate() {
                            ui.selectable_value(&mut e.ref_kind, k, *n);
                        }
                    });
                if e.ref_kind != before {
                    e.ref_key = 0;
                }
                if e.ref_kind == 1 {
                    if pins.is_empty() {
                        ui.colored_label(theme::warning(), "Click on the map to drop a pin");
                    }
                    ui.horizontal_wrapped(|ui| {
                        for &(id, c) in &pins {
                            if ui.radio(e.ref_pin == id, egui::RichText::new(format!("Pin {id}")).color(c)).clicked() {
                                e.ref_pin = id;
                                e.ref_key = 0;
                            }
                        }
                    });
                    if e.ref_pin == 0
                        && let Some(&(id, _)) = pins.first()
                    {
                        e.ref_pin = id;
                        e.ref_key = 0;
                    }
                }
                if e.spectral {
                    ui.weak("cosine of the angle between spectra: brightness does not count")
                        .on_hover_text("1 = the same spectral shape; the spectral angle (SAM) is acos of it.");
                } else {
                    ui.horizontal(|ui| {
                        if ui
                            .checkbox(&mut e.ref_fixed_year, "Fixed year")
                            .on_hover_text(
                                "Off: the reference is taken in the year shown. On: always in this year\n\
                                 (e.g. how much each place looks, year after year, like the forest of 2017).",
                            )
                            .changed()
                        {
                            e.ref_key = 0;
                        }
                        if e.ref_fixed_year {
                            let mut y = e.ref_year.min(t_n - 1);
                            egui::ComboBox::from_id_salt("emb ref year").selected_text(&info.layers[y].label).show_ui(
                                ui,
                                |ui| {
                                    for (k, l) in info.layers.iter().enumerate() {
                                        ui.selectable_value(&mut y, k, &l.label);
                                    }
                                },
                            );
                            if y != e.ref_year {
                                e.ref_year = y;
                                e.ref_key = 0;
                            }
                        }
                    });
                }
                if !e.ref_text.is_empty() {
                    ui.weak(format!("reference: {}", e.ref_text));
                }
            }
            View::Direction => {
                crate::panels::section(ui, "Direction of change");
                let cur =
                    e.change_base.map_or("Previous year".to_string(), |b| info.layers[b.min(t_n - 1)].label.clone());
                ui.horizontal(|ui| {
                    ui.label("Compared with");
                    egui::ComboBox::from_id_salt("emb dir base").selected_text(cur).show_ui(ui, |ui| {
                        ui.selectable_value(&mut e.change_base, None, "Previous year");
                        for (k, l) in info.layers.iter().enumerate() {
                            ui.selectable_value(&mut e.change_base, Some(k), &l.label);
                        }
                    });
                });
                let r = ui
                    .add(
                        egui::Slider::new(&mut e.dir_threshold, 0.0..=1.0)
                            .logarithmic(true)
                            .smallest_positive(0.005)
                            .max_decimals(3)
                            .text("changed above"),
                    )
                    .on_hover_text(
                        "The cosine distance between the two years above which a pixel changed (the\n\
                         Change view shows it); below it the pixel is grey",
                    );
                // set by hand (not by the automatic threshold arriving)
                if r.changed() && (r.dragged() || r.has_focus() || r.lost_focus() || r.is_pointer_button_down_on()) {
                    e.dir_auto = false;
                }
                ui.checkbox(&mut e.dir_auto, "Automatic: the changes unusual for this layer").on_hover_text(
                    "The median of the cosine distances of every pair of years plus 4 times\n\
                     their spread (MAD): most pixels do not change from one year to the next",
                );
                dirty |= ui
                    .radio_value(&mut e.dir_mode, 0, "Principal directions (RGB)")
                    .on_hover_text(
                        "A PCA of the difference vectors of the changed pixels of every pair of years:\n\
                         the same colour for the same kind of change, whatever the year",
                    )
                    .changed();
                dirty |= ui
                    .radio_value(&mut e.dir_mode, 1, "Like a pin's change")
                    .on_hover_text("The cosine of each change to the pin's: 1 = the same kind of change")
                    .changed();
                if e.dir_mode == 0 {
                    if let Some(b) = &e.dir_basis {
                        let pct = |c: usize| {
                            if b.total > 0.0 { 100.0 * b.eig.get(c).copied().unwrap_or(0.0) / b.total } else { 0.0 }
                        };
                        ui.weak(format!(
                            "{} changed pixels of {} pairs of years ({:.1}% of the pixels looked at); PC1-3 {:.1}% {:.1}% {:.1}% of their variance; {:.1} ms",
                            b.samples,
                            b.pairs,
                            100.0 * b.changed,
                            pct(0),
                            pct(1),
                            pct(2),
                            b.ms
                        ));
                    } else {
                        ui.weak("fitting...");
                    }
                    ui.add(egui::Slider::new(&mut e.stretch, 0.0..=10.0).text("% clipped (stretch)"));
                } else {
                    if pins.is_empty() {
                        ui.colored_label(theme::warning(), "Click on the map to drop a pin on a change");
                    }
                    ui.horizontal_wrapped(|ui| {
                        for &(id, c) in &pins {
                            if ui.radio(e.dir_pin == id, egui::RichText::new(format!("Pin {id}")).color(c)).clicked() {
                                e.dir_pin = id;
                                e.dir_ref_key = 0;
                            }
                        }
                    });
                    if !pins.iter().any(|&(id, _)| id == e.dir_pin)
                        && let Some(&(id, _)) = pins.first()
                    {
                        e.dir_pin = id;
                        e.dir_ref_key = 0;
                    }
                    let mut fixed = e.dir_ref_year.is_some();
                    if ui
                        .checkbox(&mut fixed, format!("Keep the pin's change of {}", info.layers[e.dir_ref_year.unwrap_or(t)].label))
                        .on_hover_text("Off: the pin's change in the years shown. On: its change of that year, looked for in every year")
                        .changed()
                    {
                        e.dir_ref_year = fixed.then_some(t);
                        e.dir_ref_key = 0;
                    }
                    if !e.dir_ref_text.is_empty() {
                        ui.weak(format!("reference: {}", e.dir_ref_text));
                    }
                }
            }
            View::Classes => {
                crate::panels::section(ui, "Classes");
                let mut remove = None;
                for (k, c) in e.classes.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        if ui.color_edit_button_srgba(&mut c.color).changed() {
                            dirty = true;
                        }
                        if ui.add(egui::TextEdit::singleline(&mut c.name).desired_width(110.0)).changed() {
                            dirty = true;
                        }
                        let n = c.samples.len();
                        ui.weak(format!("{n} sample{}", if n == 1 { "" } else { "s" }))
                            .on_hover_text(c.sources.join("\n"));
                        ui.menu_button("+", |ui| {
                            for &(id, col) in &pins {
                                if ui.button(egui::RichText::new(format!("Pin {id}")).color(col)).clicked() {
                                    add_samples = Some((Some(k), ClassSource::Pin(id)));
                                    ui.close();
                                }
                            }
                            if ui.add_enabled(roi, egui::Button::new("The ROI's pixels")).clicked() {
                                add_samples = Some((Some(k), ClassSource::Roi));
                                ui.close();
                            }
                        })
                        .response
                        .on_hover_text(
                            "More samples of this class: a pin, or up to 200 pixels of the ROI, in the year shown",
                        );
                        if ui.small_button("x").clicked() {
                            remove = Some(k);
                        }
                    });
                }
                if let Some(k) = remove {
                    e.classes.remove(k);
                    e.cls_gen += 1;
                    dirty = true;
                }
                egui::ComboBox::from_id_salt("emb new class")
                    .width(ui.available_width())
                    .selected_text("New class from...")
                    .show_ui(ui, |ui| {
                        for &(id, c) in &pins {
                            if ui.selectable_label(false, egui::RichText::new(format!("Pin {id}")).color(c)).clicked() {
                                add_samples = Some((None, ClassSource::Pin(id)));
                            }
                        }
                        if ui.add_enabled(roi, egui::Button::selectable(false, "The ROI's pixels")).clicked() {
                            add_samples = Some((None, ClassSource::Roi));
                        }
                        if pins.is_empty() && !roi {
                            ui.weak("Click the map to drop pins first (or draw an ROI).");
                        }
                    });
                if e.classes.is_empty() {
                    ui.weak(
                        "Drop a pin on a place of each class you know (forest, pasture, water...) and add it here.",
                    );
                }
                ui.horizontal(|ui| {
                    let before = e.cls_method;
                    egui::ComboBox::from_id_salt("emb cls method")
                        .selected_text(if e.cls_method == 1 {
                            "Nearest centroid (cosine)"
                        } else {
                            "Logistic regression (L2)"
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut e.cls_method, 0, "Logistic regression (L2)")
                                .on_hover_text("Multinomial, on standardised dimensions, each class weighing the same");
                            ui.selectable_value(&mut e.cls_method, 1, "Nearest centroid (cosine)")
                                .on_hover_text("Each class's mean direction; the score is the cosine to it");
                        });
                    if e.cls_method != before {
                        e.cls_gen += 1;
                    }
                    if e.cls_method == 0
                        && ui
                            .add(egui::DragValue::new(&mut e.cls_lambda).range(1e-4..=100.0).speed(0.01).prefix("L2 "))
                            .on_hover_text("The penalty on the weights: higher with fewer samples")
                            .changed()
                    {
                        e.cls_gen += 1;
                    }
                });
                let score = e.score_name();
                if ui
                    .add(egui::Slider::new(&mut e.cls_min, 0.0..=1.0).text(format!("minimum {score}")))
                    .on_hover_text("Below it a pixel has no class (grey): what the samples do not describe")
                    .changed()
                {
                    dirty = true;
                }
                if let Some(c) = &e.classifier {
                    let held = match c.cv_acc {
                        Some(a) if c.cv_folds == c.n.iter().sum::<usize>() => {
                            format!(", {:.0}% held out (leave-one-out)", 100.0 * a)
                        }
                        Some(a) => format!(", {:.0}% held out ({}-fold)", 100.0 * a, c.cv_folds),
                        None => ", held out: add a second sample to every class".into(),
                    };
                    ui.weak(format!(
                        "{:.0}% of the samples right{held}; fitted in {:.1} ms",
                        100.0 * c.train_acc,
                        c.ms
                    ))
                    .on_hover_text(
                        "Samples of one ROI are neighbours: the held-out share is then optimistic.\n\
                             Check the map against places you know.",
                    );
                } else if !e.cls_error.is_empty() && e.classes.len() >= 2 {
                    ui.colored_label(theme::warning(), &e.cls_error);
                }
                // the classes' share of the year shown, and the changes from another year
                if let Some(img) =
                    e.img.get(t).filter(|i| !i.counts.is_empty() && i.counts.len() == e.classes.len() + 1)
                {
                    let total: usize = img.counts.iter().sum();
                    if total > 0 {
                        egui::Grid::new("emb cls areas").num_columns(2).spacing([12.0, 2.0]).show(ui, |ui| {
                            for (k, c) in e.classes.iter().enumerate() {
                                ui.colored_label(c.color, &c.name);
                                ui.label(format!("{:.1}%", 100.0 * img.counts[k] as f64 / total as f64));
                                ui.end_row();
                            }
                            ui.weak("no class");
                            ui.label(format!("{:.1}%", 100.0 * img.counts[e.classes.len()] as f64 / total as f64));
                            ui.end_row();
                        });
                    }
                }
                if !e.spectral && t_n > 1 {
                    ui.horizontal(|ui| {
                        ui.label("Changes since");
                        let cur =
                            e.cls_from.map_or("(none)".to_string(), |y| info.layers[y.min(t_n - 1)].label.clone());
                        egui::ComboBox::from_id_salt("emb cls from").selected_text(cur).show_ui(ui, |ui| {
                            ui.selectable_value(&mut e.cls_from, None, "(none)");
                            for (k, l) in info.layers.iter().enumerate() {
                                ui.selectable_value(&mut e.cls_from, Some(k), &l.label);
                            }
                        });
                    });
                    let k = e.classes.len();
                    if let Some(y) = e.cls_from.filter(|&y| y != t && y < t_n)
                        && let (Some(a), Some(b)) = (e.img.get(y), e.img.get(t))
                        && a.classes.len() == b.classes.len()
                        && !a.classes.is_empty()
                        && a.key != 0
                        && b.key != 0
                    {
                        // from (rows) -> to (columns), the pixels with a class in both years
                        let mut m = vec![0usize; k * k];
                        for (&p, &q) in a.classes.iter().zip(&b.classes) {
                            if usize::from(p) < k && usize::from(q) < k {
                                m[usize::from(p) * k + usize::from(q)] += 1;
                            }
                        }
                        let total: usize = m.iter().sum();
                        if total > 0 {
                            ui.weak(format!(
                                "% of the pixels: {} (rows) to {} (columns)",
                                info.layers[y].label, info.layers[t].label
                            ));
                            egui::Grid::new("emb cls flows").num_columns(k + 1).spacing([8.0, 2.0]).show(ui, |ui| {
                                ui.label("");
                                for c in &e.classes {
                                    ui.colored_label(c.color, &c.name);
                                }
                                ui.end_row();
                                for (r, c) in e.classes.iter().enumerate() {
                                    ui.colored_label(c.color, &c.name);
                                    for q in 0..k {
                                        let v = 100.0 * m[r * k + q] as f64 / total as f64;
                                        let text = format!("{v:.1}");
                                        if r == q {
                                            ui.weak(text)
                                        } else {
                                            ui.label(text)
                                        };
                                    }
                                    ui.end_row();
                                }
                            });
                        }
                    } else if e.cls_from.is_some_and(|y| y != t) {
                        ui.weak("classifying that year...");
                    }
                }
            }
            View::Change => {
                crate::panels::section(ui, "Compared with");
                let cur =
                    e.change_base.map_or("Previous year".to_string(), |b| info.layers[b.min(t_n - 1)].label.clone());
                egui::ComboBox::from_id_salt("emb base").width(ui.available_width()).selected_text(cur).show_ui(
                    ui,
                    |ui| {
                        ui.selectable_value(&mut e.change_base, None, "Previous year");
                        for (k, l) in info.layers.iter().enumerate() {
                            ui.selectable_value(&mut e.change_base, Some(k), &l.label);
                        }
                    },
                );
                ui.weak("1 - cosine similarity of each pixel's vectors");
            }
            View::Band => {}
        }
        if matches!(e.view, View::Similarity | View::Change) || (e.view == View::Direction && e.dir_mode == 1) {
            let v = e.view as usize;
            egui::ComboBox::from_id_salt("emb cmap")
                .width(ui.available_width())
                .selected_text(janus_gpu::colormaps::all()[e.cmap[v]].name)
                .show_ui(ui, |ui| {
                    for (k, c) in janus_gpu::colormaps::all().iter().enumerate() {
                        if ui.selectable_value(&mut e.cmap[v], k, c.name).changed() {
                            dirty = true;
                        }
                    }
                });
            ui.horizontal(|ui| {
                let r = &mut e.range[v];
                let (mut lo, mut hi) = (r.lo, r.hi);
                let a = ui.add(egui::DragValue::new(&mut lo).speed(0.002).range(-1.0..=2.0).max_decimals(3));
                let b = ui.add(egui::DragValue::new(&mut hi).speed(0.002).range(-1.0..=2.0).max_decimals(3));
                if a.changed() || b.changed() {
                    (r.lo, r.hi, r.manual) = (lo, hi.max(lo + 1e-4), true);
                    dirty = true;
                }
                if ui.button("Auto").on_hover_text("2-99.5% (similarity) or 2-98% (change) of the year shown").clicked()
                {
                    (r.manual, r.key) = (false, 0);
                    if let Some(img) = e.img.get(t).filter(|i| !i.rgba && i.tex.is_some()) {
                        (r.lo, r.hi) = (img.lo, img.hi);
                    }
                    dirty = true;
                }
            });
        }
        if e.drawn() {
            ui.horizontal(|ui| {
                if ui
                    .button("Export GeoTIFF: visible area")
                    .on_hover_text("The view at full resolution, read from the files")
                    .clicked()
                {
                    export = Some(false);
                }
                if ui.button("Whole layer").clicked() {
                    export = Some(true);
                }
            });
        }
        // charts: the similarity of the cursor and the pins to the reference, year by year; their vectors in the
        // year shown
        if let Some(s) = e.store.clone().filter(|s| s.ranges_ready()) {
            if !e.reference.is_empty() && e.view == View::Similarity && !e.spectral {
                crate::panels::section(ui, "Similarity over the years");
                let rn = e.reference.iter().map(|&v| f64::from(v) * f64::from(v)).sum::<f64>().sqrt();
                let mut v = vec![0f32; s.d];
                Plot::new("emb sim").height(170.0).y_axis_label("cosine").allow_drag(false).allow_scroll(false).show(
                    ui,
                    |plot| {
                        for (label, p, c) in &probes {
                            let mut pts = Vec::new();
                            for y in 0..t_n {
                                if !s.year_ready(y) || !s.valid(y, *p) || s.norm(y, *p) <= 0.0 {
                                    continue;
                                }
                                s.dequant(y, *p, &mut v);
                                let dot: f64 =
                                    v.iter().zip(&e.reference).map(|(&a, &b)| f64::from(a) * f64::from(b)).sum();
                                pts.push([info.decimal_year(y), dot / (rn * f64::from(s.norm(y, *p)))]);
                            }
                            plot.line(Line::new(label, PlotPoints::from(pts.clone())).color(theme::on_plot(*c)));
                            plot.points(
                                egui_plot::Points::new(label, PlotPoints::from(pts))
                                    .color(theme::on_plot(*c))
                                    .radius(3.0),
                            );
                        }
                        plot.vline(VLine::new("", info.decimal_year(t)).color(Color32::from_white_alpha(90)));
                    },
                );
            }
            if s.year_ready(t) && !probes.is_empty() && !e.spectral {
                crate::panels::section(ui, "Latent profile");
                ui.weak(format!(
                    "The {} values of each vector in {} (no physical meaning: compare their shapes)",
                    s.d, info.layers[t].label
                ));
                let mut v = vec![0f32; s.d];
                Plot::new("emb prof")
                    .height(150.0)
                    .x_axis_label("dimension")
                    .allow_drag(false)
                    .allow_scroll(false)
                    .show(ui, |plot| {
                        for (label, p, c) in &probes {
                            if !s.valid(t, *p) {
                                continue;
                            }
                            s.dequant(t, *p, &mut v);
                            let pts: Vec<[f64; 2]> =
                                v.iter().enumerate().map(|(k, &x)| [k as f64, f64::from(x)]).collect();
                            plot.line(Line::new(label, PlotPoints::from(pts)).color(theme::on_plot(*c)));
                        }
                    });
            }
        }
        ui.add_space(6.0);
        egui::CollapsingHeader::new("Download embeddings")
            .id_salt("emb dl panel")
            .show(ui, |ui| self.embedding_download_ui(ui));
        if let Some((to, from)) = add_samples {
            let pin_colour = match from {
                ClassSource::Pin(id) => self.series.pins.iter().find(|p| p.id == id).map(|p| p.color),
                ClassSource::Roi => None,
            };
            match self.class_samples(i, from) {
                Some((v, source)) => {
                    let e = self.layers[i].emb.as_mut().unwrap();
                    let k = match to {
                        Some(k) if k < e.classes.len() => k,
                        _ => {
                            let n = e.classes.len();
                            e.classes.push(EmbClass {
                                name: format!("Class {}", n + 1),
                                color: pin_colour.unwrap_or(CLASS_COLOURS[n % CLASS_COLOURS.len()]),
                                samples: Vec::new(),
                                sources: Vec::new(),
                            });
                            n
                        }
                    };
                    e.classes[k].samples.extend(v);
                    e.classes[k].sources.push(source);
                    e.cls_gen += 1;
                    dirty = true;
                }
                None => {
                    self.error_popup = Some("No vector there in the year shown (outside the layer, or no data).".into())
                }
            }
        }
        if dirty {
            self.map.dirty = true;
        }
        if let Some(whole) = export {
            match self.embedding_export_spec(whole) {
                Ok(spec) => {
                    let li = self.layers[i].name.clone();
                    let year = spec.info.layers[spec.t].label.clone();
                    let kind = match spec.kind {
                        ExportKind::Pca => "pca",
                        ExportKind::Similarity => "similarity",
                        ExportKind::Change => "change",
                        ExportKind::Direction => "direction",
                        ExportKind::ChangeLike => "change_like",
                        ExportKind::Classes => "classes",
                    };
                    let file = crate::exports::sanitize(&format!("{li}_{year}_{kind}.tif"));
                    let mut dlg = rfd::FileDialog::new().set_file_name(file).add_filter("GeoTIFF", &["tif"]);
                    let folder = self.export_folder();
                    if !folder.is_empty() {
                        dlg = dlg.set_directory(folder);
                    }
                    if let Some(p) = dlg.save_file() {
                        let p = p.to_string_lossy().into_owned();
                        self.remember_export_folder(&p);
                        self.export_embeddings(p, spec, ui.ctx());
                    }
                }
                Err(why) => self.error_popup = Some(why),
            }
        }
    }
}

/// Writes an embedding view as a GeoTIFF (src/app_export.cpp, exportEmbeddings): the layer's grid and CRS, NaN as
/// nodata, a description per band, the view's metadata; through a temporary file.
fn write_embedding_tiff(path: &str, e: &EmbeddingExport, threads: usize, j: &ExportJob) -> Result<(), String> {
    use gdal::raster::{Buffer, RasterCreationOptions};
    let info = &e.info;
    let [x0, y0, x1, y1] = e.win;
    let (w, h, k) = (x1 - x0, y1 - y0, e.bands());
    let drv = gdal::DriverManager::get_driver_by_name("GTiff").map_err(|er| er.to_string())?;
    let n_threads = threads.to_string();
    let opts = RasterCreationOptions::from_iter([
        "TILED=YES",
        "COMPRESS=DEFLATE",
        "PREDICTOR=3",
        "INTERLEAVE=BAND",
        "BIGTIFF=IF_SAFER",
        &format!("NUM_THREADS={n_threads}"),
    ]);
    let tmp = format!("{path}.part");
    let mut out = drv
        .create_with_band_type_with_options::<f32, _>(&tmp, w, h, k, &opts)
        .map_err(|er| format!("could not create the file: {er}"))?;
    if let Some(g) = info.geo_transform {
        // the layer's own grid and CRS
        let (fx, fy) = (x0 as f64, y0 as f64);
        out.set_geo_transform(&[g[0] + fx * g[1] + fy * g[2], g[1], g[2], g[3] + fx * g[4] + fy * g[5], g[4], g[5]])
            .map_err(|er| er.to_string())?;
    }
    if !info.crs_wkt.is_empty() {
        let _ = out.set_projection(&info.crs_wkt);
    }
    for b in 0..k {
        let mut band = out.rasterband(b + 1).map_err(|er| er.to_string())?;
        let _ = band.set_no_data_value(Some(f64::NAN));
        if let Some(n) = e.band_names.get(b) {
            let _ = band.set_description(n);
        }
    }
    use gdal::Metadata;
    for (key, value) in &e.metadata {
        let _ = out.set_metadata_item(key, value, "");
    }
    let src = std::path::absolute(&info.layers[e.t].path)
        .map_or(info.layers[e.t].path.clone(), |p| p.to_string_lossy().into_owned());
    let _ = out.set_metadata_item("JANUS_SOURCE", &src, "");
    let mut write_err = None;
    let r = janus_embed::compute_embedding_export(e, threads, &mut |row, n, data| {
        let plane = w * n;
        for b in 0..k {
            let mut band = match out.rasterband(b + 1) {
                Ok(bd) => bd,
                Err(er) => {
                    write_err = Some(er.to_string());
                    return false;
                }
            };
            let mut buf = Buffer::new((w, n), data[b * plane..(b + 1) * plane].to_vec());
            if let Err(er) = band.write((0, row as isize), (w, n), &mut buf) {
                write_err = Some(er.to_string());
                return false;
            }
        }
        j.set_progress((row + n) as f64 / h as f64);
        !j.cancelled()
    });
    drop(out);
    if let Some(er) = write_err {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("could not write the file: {er}"));
    }
    if let Err(er) = r {
        let _ = std::fs::remove_file(&tmp);
        return Err(if er.is_empty() { "cancelled".into() } else { er });
    }
    let _ = std::fs::remove_file(path);
    std::fs::rename(&tmp, path).map_err(|er| format!("cannot rename {tmp}: {er}"))
}
