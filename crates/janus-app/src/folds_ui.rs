//! Spatial cross-validation in the Classification window (janus-ml folds.rs; IDEIAS.md, "Validação cruzada espacial,
//! como o blockCV"): how the training holds out places (whole places at random, spatial blocks, a buffer around the
//! places tested, clusters of the values, kNNDM), the places as points in metres, the blocks' size from the range
//! of the spatial autocorrelation of the values, and the folds on the map: a GeoPackage of the blocks and the
//! places, opened as a vector layer coloured by fold.

use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};

use egui::{Color32, ComboBox, Ui};
use egui_plot::{Legend, Line, Plot, PlotPoints};
use janus_cube::CubeInfo;
use janus_geo::vectors::{Feature, Kind, VectorData};
use janus_ml::folds::{self, Clustering, FoldSpec, Folds, Method, Selection, Shape};
use janus_ml::{InputBand, Series, TrainConfig};
use serde_json::{Value, json};

use crate::app::State;
use crate::ml::Site;
use crate::vectors::VectorLayer;

/// The places' frame: metres (the pixel's size; for a geographic CRS, in metres at the image's middle latitude),
/// or pixels without a CRS. y grows to the north.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Frame {
    pub mx: f64,
    pub my: f64,
    pub metres: bool,
}

impl Frame {
    pub fn of(info: &CubeInfo) -> Frame {
        let pixels = Frame { mx: 1.0, my: 1.0, metres: false };
        let Some(gt) = info.geo_transform else { return pixels };
        let Some(srs) =
            (!info.crs_wkt.is_empty()).then(|| gdal::spatial_ref::SpatialRef::from_wkt(&info.crs_wkt).ok()).flatten()
        else {
            return pixels;
        };
        let (sx, sy) = (gt[1].hypot(gt[4]), gt[2].hypot(gt[5]));
        if srs.is_geographic() {
            let lat = gt[3] + 0.5 * info.height as f64 * gt[5];
            Frame { mx: sx * 111_320.0 * lat.to_radians().cos(), my: sy * 110_574.0, metres: true }
        } else {
            let u = srs.linear_units();
            Frame { mx: sx * u, my: sy * u, metres: true }
        }
    }

    pub fn to_m(self, px: f64, py: f64) -> [f64; 2] {
        [px * self.mx, -py * self.my]
    }

    pub fn to_px(self, p: [f64; 2]) -> (f64, f64) {
        (p[0] / self.mx, -p[1] / self.my)
    }

    pub fn unit(self) -> &'static str {
        if self.metres { "m" } else { "pixels" }
    }
}

/// Each place's point: the middle of its pixels.
pub(crate) fn place_points(sites: &[Site], frame: Frame) -> Vec<[f64; 2]> {
    sites
        .iter()
        .map(|s| {
            let n = s.pixels.len().max(1) as f64;
            let (x, y) = s.pixels.iter().fold((0.0, 0.0), |a, &(x, y)| (a.0 + x as f64 + 0.5, a.1 + y as f64 + 0.5));
            frame.to_m(x / n, y / n)
        })
        .collect()
}

/// Pixels drawn for the semivariograms (blockCV's `cv_spatial_autocor` draws 5000; the series is read at each)
const RANGE_PIXELS: usize = 2000;
/// Dates read for them at most (evenly over the series)
const RANGE_DATES: usize = 12;

/// The range of the spatial autocorrelation of the values the model reads: semivariograms of each value on some
/// dates at pixels drawn at random, the median of their ranges (and how many fitted).
pub(crate) fn autocorrelation_range(
    info: &Arc<CubeInfo>,
    inputs: &[InputBand],
    frame: Frame,
    threads: usize,
) -> Result<(f64, usize), String> {
    let (w, h) = (info.width, info.height);
    if w < 4 || h < 4 {
        return Err("the image is too small for a semivariogram: give the size".into());
    }
    let mut rng = janus_ml::train::Rng::new(7);
    let want = RANGE_PIXELS.min(w * h);
    let mut seen = std::collections::HashSet::new();
    let mut pixels = Vec::with_capacity(want);
    while pixels.len() < want {
        let p = ((rng.next_u64() % w as u64) as usize, (rng.next_u64() % h as u64) as usize);
        if seen.insert(p) {
            pixels.push(p);
        }
    }
    let t = info.t();
    let mut dates: Vec<usize> =
        (0..RANGE_DATES.min(t)).map(|i| if t <= RANGE_DATES { i } else { i * (t - 1) / (RANGE_DATES - 1) }).collect();
    dates.dedup();
    let raw = janus_ml::data::read_points_on(info, inputs, &pixels, &dates);
    let cols = raw.t * raw.b;
    let columns: Vec<Vec<f64>> =
        (0..cols).map(|j| (0..raw.n).map(|i| raw.values[i * cols + j] as f64).collect()).collect();
    let (x, y): (Vec<f64>, Vec<f64>) = pixels
        .iter()
        .map(|&(px, py)| {
            let m = frame.to_m(px as f64 + 0.5, py as f64 + 0.5);
            (m[0], m[1])
        })
        .unzip();
    let cutoff = 0.35 * (w as f64 * frame.mx).hypot(h as f64 * frame.my);
    let (median, ranges) = folds::autocorrelation_range(&x, &y, &columns, cutoff, threads);
    if !median.is_finite() {
        return Err("no semivariogram fits the values (too few pixels with a value?): give the size".into());
    }
    Ok((median, ranges.iter().filter(|r| r.is_finite()).count()))
}

/// The places dealt into folds by `spec`: the blocks' size (or the buffer's) from the range of the spatial
/// autocorrelation when it is 0; clusters of the places' mean values (`values`: the samples' series and places,
/// else read now). `note` says what was chosen.
#[allow(clippy::too_many_arguments)]
pub(crate) fn make_folds(
    info: &Arc<CubeInfo>,
    inputs: &[InputBand],
    sites: &[Site],
    classes: &[String],
    spec: &FoldSpec,
    values: Option<(&Series, &[usize])>,
    min_valid: f64,
    seed: u64,
    threads: usize,
    note: &dyn Fn(&str),
) -> Result<Folds, String> {
    let frame = Frame::of(info);
    let xy = place_points(sites, frame);
    let n = xy.len();
    if n < 2 {
        return Err("at least two places are needed for folds".into());
    }
    let k = spec.k.clamp(2, n);
    let class_of: Vec<u32> =
        sites.iter().map(|s| classes.iter().position(|c| c == &s.class).unwrap_or(0) as u32).collect();
    let size = |what: &str| -> Result<f64, String> {
        if spec.size > 0.0 {
            return Ok(spec.size);
        }
        let (r, fitted) = autocorrelation_range(info, inputs, frame, threads)?;
        note(&format!(
            "{what} of {r:.0} {}: the median range of the spatial autocorrelation of {fitted} values (semivariograms \
             of {RANGE_PIXELS} pixels)",
            frame.unit()
        ));
        Ok(r)
    };
    let f = match spec.method {
        Method::Places => return Err("whole places at random: no folds".into()),
        Method::Blocks => {
            let s = size("blocks")?;
            folds::blocks(&xy, k, s, spec.shape, spec.selection, Some(&class_of), spec.iterations, seed)?
        }
        Method::Buffer => folds::buffer(&xy, size("a buffer")?)?,
        Method::Clusters => {
            let owned;
            let (x, place) = match values {
                Some(v) => v,
                None => {
                    owned = crate::ml::sample_series(info, inputs, sites, classes, min_valid);
                    (&owned.0, &owned.4[..])
                }
            };
            let p = x.t * x.b;
            let (mut sum, mut count) = (vec![0f64; n * p], vec![0usize; n]);
            for (i, &pl) in place.iter().enumerate() {
                count[pl] += 1;
                for j in 0..p {
                    let v = x.values[i * p + j];
                    if v.is_finite() {
                        sum[pl * p + j] += v as f64;
                    }
                }
            }
            let with: Vec<usize> = (0..n).filter(|&i| count[i] > 0).collect();
            let vals: Vec<f64> = with
                .iter()
                .flat_map(|&i| (0..p).map(move |j| (i, j)))
                .map(|(i, j)| sum[i * p + j] / count[i] as f64)
                .collect();
            let c = folds::clusters(&vals, with.len(), p, k.min(with.len()), seed)?;
            let mut fold = vec![-1; n];
            for (j, &i) in with.iter().enumerate() {
                fold[i] = c.fold[j];
            }
            Folds { fold, ..c }
        }
        Method::Knndm => {
            let (w, h) = (info.width, info.height);
            let step = ((w * h) as f64 / 1000.0).sqrt().max(1.0);
            let mut pred = vec![];
            let mut gy = step / 2.0;
            while gy < h as f64 {
                let mut gx = step / 2.0;
                while gx < w as f64 {
                    pred.extend_from_slice(&frame.to_m(gx, gy));
                    gx += step;
                }
                gy += step;
            }
            let train: Vec<f64> = xy.iter().flatten().copied().collect();
            let f = folds::knndm(
                &train,
                n,
                2,
                &pred,
                pred.len() / 2,
                k,
                spec.clustering,
                spec.maxp,
                spec.candidates,
                seed,
                threads,
            )?;
            note(&format!(
                "kNNDM: {} clusters merged into {k} folds, W {:.1} {} (a random {k}-fold's {:.1})",
                f.q,
                f.w,
                frame.unit(),
                folds::random_w(&train, n, 2, k, &f.gij, seed)
            ));
            f
        }
    };
    if spec.method == Method::Blocks {
        note(&format!("{} blocks hold places", f.blocks.len()));
    }
    Ok(f)
}

/// The validation's split: whole places at random (`folds` None), or by the folds (the held-out fold, or the
/// buffer's places and their neighbours out of the training); how, the model's "split".
pub(crate) fn holdout(
    folds: Option<&Folds>,
    places: &[usize],
    y: &[u32],
    classes: usize,
    cfg: &TrainConfig,
) -> (Vec<usize>, Vec<usize>, &'static str) {
    match folds {
        None => janus_ml::train::split_by_place(y, Some(places), classes, cfg.validation_split, cfg.seed),
        Some(f) => {
            let (a, b) = folds::holdout(f, places, y, classes, cfg.validation_split, cfg.seed);
            (a, b, f.method.name())
        }
    }
}

/// Each sample's fold (-1: its place in none).
pub(crate) fn sample_folds(f: &Folds, places: &[usize]) -> Vec<i64> {
    places.iter().map(|&p| f.fold.get(p).copied().unwrap_or(-1)).collect()
}

/// The groups of the area of applicability's dissimilarity (each sample's to the samples of the other groups, as
/// CAST's with the folds of the cross-validation): the folds', a place in none its own; else the places.
pub(crate) fn aoa_groups(folds: Option<&Folds>, places: &[usize]) -> Vec<usize> {
    match folds.filter(|f| f.method.has_folds()) {
        Some(f) => places
            .iter()
            .map(|&p| match f.fold.get(p).copied().unwrap_or(-1) {
                v if v >= 0 => v as usize,
                _ => f.k + p,
            })
            .collect(),
        None => places.to_vec(),
    }
}

/// What a model keeps of its folds.
pub(crate) fn summary(f: &Folds, spec: &FoldSpec, frame: Frame) -> Value {
    let mut v = json!({
        "method": f.method.name(),
        "k": f.k,
        "places": f.counts(),
        "unit": frame.unit(),
    });
    if matches!(f.method, Method::Blocks | Method::Buffer) {
        v["size"] = json!(f.size);
    }
    if f.method == Method::Blocks {
        v["shape"] = json!(spec.shape);
        v["selection"] = json!(spec.selection);
        v["blocks"] = json!(f.blocks.len());
    }
    if f.method == Method::Knndm {
        v["clustering"] = json!(spec.clustering);
        v["w"] = json!(f.w);
        v["q"] = json!(f.q);
    }
    v
}

/// A fold's colour (a place in none: grey).
pub(crate) fn fold_color(f: i64) -> Color32 {
    if f < 0 {
        return Color32::GRAY;
    }
    let [r, g, b] = crate::ml::class_color(f as usize);
    Color32::from_rgb(r, g, b)
}

/// The folds as features in the series' CRS: the blocks (areas), then the places (points); fields what, fold
/// (from 1; "" in none) and places.
fn features(f: &Folds, xy: &[[f64; 2]], sites: &[Site], frame: Frame, info: &CubeInfo) -> Vec<Feature> {
    let geo = |p: [f64; 2]| -> [f64; 2] {
        let (px, py) = frame.to_px(p);
        info.pixel_to_geo(px, py).map_or([px, py], |(x, y)| [x, y])
    };
    let fold_text = |v: i64| if v >= 0 { (v + 1).to_string() } else { String::new() };
    let mut out = vec![];
    for b in &f.blocks {
        let ring: Vec<f64> = b.ring.iter().flat_map(|&p| geo(p)).collect();
        let n = b.ring.len().saturating_sub(1).max(1) as f64;
        let c = b.ring.iter().take(n as usize).fold([0.0, 0.0], |a, p| [a[0] + p[0] / n, a[1] + p[1] / n]);
        let c = geo(c);
        out.push(Feature {
            fid: out.len() as i64 + 1,
            kind: Kind::Area,
            parts: vec![ring],
            cx: c[0],
            cy: c[1],
            values: vec!["block".into(), fold_text(b.fold as i64), b.points.to_string()],
        });
    }
    for (i, p) in xy.iter().enumerate() {
        let g = geo(*p);
        out.push(Feature {
            fid: out.len() as i64 + 1,
            kind: Kind::Point,
            parts: vec![g.to_vec()],
            cx: g[0],
            cy: g[1],
            values: vec![
                sites.get(i).map_or(String::new(), |s| s.label.clone()),
                fold_text(f.fold.get(i).copied().unwrap_or(-1)),
                "1".into(),
            ],
        });
    }
    out
}

/// The features written as a GeoPackage (layer "folds"), in the series' CRS.
fn write_gpkg(path: &std::path::Path, features: &[Feature], crs_wkt: &str) -> Result<(), String> {
    use gdal::vector::{FieldDefn, Geometry, LayerAccess, LayerOptions, OGRFieldType, OGRwkbGeometryType};
    let _ = std::fs::remove_file(path);
    let drv = gdal::DriverManager::get_driver_by_name("GPKG").map_err(|e| e.to_string())?;
    let mut ds = drv.create_vector_only(path).map_err(|e| e.to_string())?;
    let srs = (!crs_wkt.is_empty()).then(|| gdal::spatial_ref::SpatialRef::from_wkt(crs_wkt).ok()).flatten();
    let l = ds
        .create_layer(LayerOptions {
            name: "folds",
            srs: srs.as_ref(),
            ty: OGRwkbGeometryType::wkbUnknown,
            options: None,
        })
        .map_err(|e| e.to_string())?;
    for (name, ty) in
        [("what", OGRFieldType::OFTString), ("fold", OGRFieldType::OFTString), ("places", OGRFieldType::OFTInteger)]
    {
        FieldDefn::new(name, ty).and_then(|d| d.add_to_layer(&l)).map_err(|e| e.to_string())?;
    }
    for f in features {
        let wkt = match f.kind {
            Kind::Point => format!("POINT ({} {})", f.cx, f.cy),
            _ => format!(
                "POLYGON (({}))",
                f.parts[0].chunks(2).map(|p| format!("{} {}", p[0], p[1])).collect::<Vec<_>>().join(", ")
            ),
        };
        let g = Geometry::from_wkt(&wkt).map_err(|e| e.to_string())?;
        let mut feat = gdal::vector::Feature::new(l.defn()).map_err(|e| e.to_string())?;
        feat.set_geometry(g).map_err(|e| e.to_string())?;
        feat.set_field_string(0, &f.values[0]).map_err(|e| e.to_string())?;
        feat.set_field_string(1, &f.values[1]).map_err(|e| e.to_string())?;
        feat.set_field_integer(2, f.values[2].parse().unwrap_or(0)).map_err(|e| e.to_string())?;
        feat.create(&l).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The folds written beside a model (or for the map): the GeoPackage and its features.
pub(crate) fn save_folds(
    path: &std::path::Path,
    f: &Folds,
    sites: &[Site],
    info: &CubeInfo,
) -> Result<Vec<Feature>, String> {
    let frame = Frame::of(info);
    let xy = place_points(sites, frame);
    let feats = features(f, &xy, sites, frame, info);
    write_gpkg(path, &feats, &info.crs_wkt)?;
    Ok(feats)
}

/// Folds made for the map: the folds, their file and features, what was chosen.
pub(crate) struct Shown {
    pub folds: Folds,
    pub path: String,
    pub features: Vec<Feature>,
    pub notes: Vec<String>,
    pub unit: &'static str,
    pub crs_wkt: String,
}

/// The folds on the map: the last made, the one being made, its layer.
#[derive(Default)]
pub struct FoldsView {
    pub shown: Option<Shown>,
    pending: Option<Receiver<Result<Shown, String>>>,
    pub layer: Option<u32>,
    pub error: String,
}

impl FoldsView {
    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }
}

/// What each way of holding out does.
fn method_help(m: Method) -> &'static str {
    match m {
        Method::Places => {
            "A share of the places (a polygon's pixels, a pin) at random, by class: the validation meets places the \
             model never saw, but maybe next to those it did"
        }
        Method::Blocks => {
            "The places in blocks of a grid, the blocks dealt into folds (blockCV's cv_spatial): a fold's places are \
             away from the others'. The size: at least the range of the spatial autocorrelation (0: found from the \
             values)"
        }
        Method::Buffer => {
            "A share of the places held out, and the places within the distance of them out of the training too \
             (blockCV's cv_buffer)"
        }
        Method::Clusters => {
            "k-means of the places' values: a fold's places are unlike the others', a validation of conditions the \
             model has not seen (blockCV's cv_cluster)"
        }
        Method::Knndm => {
            "The folds whose distances from each place to the nearest place of another fold are distributed most \
             like the distances from the map to the nearest place (Linnenbrink et al. 2024, CAST's knndm): the \
             validation as far from the training as the map is"
        }
    }
}

impl State {
    /// The validation's settings (both the forest's and the networks'): how the places are held out, the folds'
    /// settings, and a button that shows them on the map.
    pub(crate) fn folds_settings_ui(&mut self, ui: &mut Ui, info: &Arc<CubeInfo>) {
        let forest = self.ml.arch == crate::ml::FOREST;
        let unit = Frame::of(info).unit();
        let spec = &mut self.ml.cfg.folds;
        ui.weak(
            "How the accuracy is measured: on samples the model did not learn from. Samples next to those it learned              from are easy, and the accuracy they give is too good for the map far from them; kNNDM (the default)              tests on places as far from the training as the map is.",
        );
        let label = |m: Method| {
            if m == Method::Knndm { format!("{} (recommended)", m.label()) } else { m.label().to_string() }
        };
        egui::Grid::new("ml folds").num_columns(2).show(ui, |ui| {
            ui.label("Validation").on_hover_text(method_help(spec.method));
            ComboBox::from_id_salt("ml folds method")
                .selected_text(label(spec.method))
                .width(crate::fit::scaled(ui, 260.0))
                .show_ui(ui, |ui| {
                    for m in Method::ALL {
                        ui.selectable_value(&mut spec.method, m, label(m)).on_hover_text(method_help(m));
                    }
                });
            ui.end_row();
            if spec.method.has_folds() {
                ui.label("Folds");
                ui.add(egui::DragValue::new(&mut spec.k).range(2..=20));
                ui.end_row();
            }
            if matches!(spec.method, Method::Blocks | Method::Buffer) {
                ui.label(if spec.method == Method::Blocks { "Blocks' size" } else { "Distance" }).on_hover_text(
                    "0: the range of the spatial autocorrelation of the values read (the median of semivariograms \
                     of pixels drawn at random, blockCV's cv_spatial_autocor)",
                );
                ui.add(egui::DragValue::new(&mut spec.size).range(0.0..=1e7).speed(10.0).custom_formatter(
                    move |v, _| {
                        if v == 0.0 { "from the autocorrelation".into() } else { format!("{v:.0} {unit}") }
                    },
                ));
                ui.end_row();
            }
            if spec.method == Method::Blocks {
                ui.label("Blocks");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut spec.shape, Shape::Hexagon, "hexagons");
                    ui.selectable_value(&mut spec.shape, Shape::Square, "squares");
                });
                ui.end_row();
                ui.label("Dealt").on_hover_text(
                    "At random: the most even of 100 draws (each fold near a k-th of each class's places). In rows: \
                     one after another. A checkerboard: squares, 2 folds",
                );
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut spec.selection, Selection::Random, "at random");
                    ui.selectable_value(&mut spec.selection, Selection::Systematic, "in rows");
                    ui.selectable_value(&mut spec.selection, Selection::Checkerboard, "checkerboard");
                });
                ui.end_row();
                if spec.selection == Selection::Checkerboard {
                    (spec.shape, spec.k) = (Shape::Square, 2);
                }
            }
            if spec.method == Method::Knndm {
                ui.label("Clusters").on_hover_text(
                    "The places grouped by Ward's hierarchical clustering (up to 4000 places; k-means beyond) or \
                     k-means, for numbers of groups from the folds to the places",
                );
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut spec.clustering, Clustering::Hierarchical, "Ward's");
                    ui.selectable_value(&mut spec.clustering, Clustering::Kmeans, "k-means");
                });
                ui.end_row();
            }
        });
        if spec.method.has_folds() {
            if forest {
                ui.weak(
                    "Cross-validated: a forest for each fold, grown on the others, predicts it; the forest kept is \
                     grown on all the samples.",
                );
            } else {
                let k = spec.k;
                ui.checkbox(
                    &mut self.ml.cfg.cross_validate,
                    format!("Cross-validate: a network for each fold ({} trainings)", k + 1),
                )
                .on_hover_text(
                    "Each fold predicted by a network trained on the other folds (each stopping early on whole \
                         places of its own folds): the accuracy of every sample on places the network never saw, \
                         and the error profile of the area of applicability from them. Then the network kept, \
                         validating on one fold.",
                );
                ui.weak(if self.ml.cfg.cross_validate {
                    "The accuracy is the cross-validation's; the network kept validates (and stops early) on one fold."
                } else {
                    "The network validates (and stops early) on one fold: the one whose classes the others keep best."
                });
            }
        }
        if spec.method != Method::Places {
            ui.horizontal_wrapped(|ui| {
                let busy = self.ml.folds_view.busy();
                if ui.add_enabled(!busy, egui::Button::new("Show the folds on the map")).clicked() {
                    self.show_folds(info, ui.ctx());
                }
                if busy {
                    ui.spinner();
                }
            });
        }
        let spec_method = self.ml.cfg.folds.method;
        let v = &self.ml.folds_view;
        if !v.error.is_empty() {
            ui.colored_label(crate::theme::error(), &v.error);
        }
        // the folds shown of the way chosen (not those of another, made before)
        if let Some(s) = v.shown.as_ref().filter(|s| s.folds.method == spec_method) {
            folds_summary_ui(ui, s);
        }
    }

    /// Makes the folds of the samples now (on a thread: the range of the autocorrelation reads the series) and
    /// shows them on the map.
    pub(crate) fn show_folds(&mut self, info: &Arc<CubeInfo>, ctx: &egui::Context) {
        let (sites, classes) = match self.ml_sites() {
            Ok(s) => s,
            Err(e) => {
                self.ml.folds_view.error = e;
                return;
            }
        };
        self.ml.folds_view.error.clear();
        let inputs = self.ml_inputs(info);
        let (spec, seed, min_valid) = (self.ml.cfg.folds.clone(), self.ml.cfg.seed, self.ml.min_valid);
        let threads = self.processing_threads();
        let dir = self.results_dir().join("folds");
        let name = format!(
            "{}_{}_{}.gpkg",
            crate::zeit::series_name(info, &self.layers.get(self.active).map(|l| l.inputs.clone()).unwrap_or_default()),
            spec.method.name(),
            crate::zeit::timestamp()
        );
        let (tx, rx) = channel();
        let (info, c) = (info.clone(), ctx.clone());
        std::thread::Builder::new()
            .name("ml-folds".into())
            .spawn(move || {
                let notes = Mutex::new(vec![]);
                let r = crate::ml::guarded(|| {
                    let f =
                        make_folds(&info, &inputs, &sites, &classes, &spec, None, min_valid, seed, threads, &|m| {
                            notes.lock().unwrap().push(m.to_string())
                        })?;
                    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                    let path = dir.join(&name);
                    let features = save_folds(&path, &f, &sites, &info)?;
                    Ok(Shown {
                        folds: f,
                        path: path.to_string_lossy().into_owned(),
                        features,
                        notes: notes.into_inner().unwrap(),
                        unit: Frame::of(&info).unit(),
                        crs_wkt: info.crs_wkt.clone(),
                    })
                });
                let _ = tx.send(r);
                c.request_repaint();
            })
            .ok();
        self.ml.folds_view.pending = Some(rx);
    }

    /// Folds made: their layer (replaced, if it is still open).
    pub(crate) fn pump_folds(&mut self) {
        let Some(rx) = &self.ml.folds_view.pending else { return };
        let r = match rx.try_recv() {
            Ok(r) => r,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(_) => Err("the folds were not made".into()),
        };
        self.ml.folds_view.pending = None;
        match r {
            Ok(s) => {
                self.folds_layer(&s);
                self.ml.folds_view.shown = Some(s);
            }
            Err(e) => self.ml.folds_view.error = e,
        }
    }

    fn folds_layer(&mut self, s: &Shown) {
        let crs_name =
            gdal::spatial_ref::SpatialRef::from_wkt(&s.crs_wkt).ok().and_then(|r| r.name()).unwrap_or_default();
        let data = VectorData {
            path: s.path.clone(),
            layer: "folds".into(),
            crs_wkt: s.crs_wkt.clone(),
            crs_name,
            fields: vec!["what".into(), "fold".into(), "places".into()],
            points: s.features.iter().filter(|f| f.kind == Kind::Point).count(),
            areas: s.features.iter().filter(|f| f.kind == Kind::Area).count(),
            vertices: s.features.iter().map(|f| f.parts.iter().map(|p| p.len() / 2).sum::<usize>()).sum(),
            features: s.features.clone(),
            ..Default::default()
        };
        let colors: Vec<Color32> =
            s.features.iter().map(|f| fold_color(f.values[1].parse::<i64>().map_or(-1, |v| v - 1))).collect();
        let name = format!("Folds: {}", s.folds.method.label());
        if let Some(i) = self.ml.folds_view.layer.and_then(|id| self.vector_index(id)) {
            let v = &mut self.vectors[i];
            (v.data, v.name, v.colors, v.placed_for) = (Arc::new(data), name, colors, 0);
            v.visible = true;
            self.place_vector(i);
            self.map.dirty = true;
            return;
        }
        let z = self.take_z();
        let id = self.next_vector_id;
        self.next_vector_id += 1;
        self.vectors.push(VectorLayer {
            id,
            data: Arc::new(data),
            name,
            color: Color32::GRAY,
            visible: true,
            px: Default::default(),
            boxes: Vec::new(),
            anchor: Vec::new(),
            placed_for: 0,
            why: String::new(),
            off_map: false,
            colors,
            gpu: Default::default(),
            placing: None,
            z,
            attrs: Default::default(),
        });
        self.ml.folds_view.layer = Some(id);
        let i = self.vectors.len() - 1;
        self.place_vector(i);
        self.map.dirty = true;
    }
}

/// The folds made: each fold's places in its colour, what was chosen, and kNNDM's distances (CAST's plot of
/// knndm: the distribution functions of the map's to the nearest place, the places' to the nearest other, and to
/// the nearest of another fold).
fn folds_summary_ui(ui: &mut Ui, s: &Shown) {
    let f = &s.folds;
    if f.method == Method::Buffer {
        let mean = f.exclude.iter().map(Vec::len).sum::<usize>() as f64 / f.exclude.len().max(1) as f64;
        ui.label(format!(
            "{} places; within {:.0} {} of a place, {mean:.1} others on average (out of the training when it is tested)",
            f.fold.len(),
            f.size,
            s.unit
        ));
    } else {
        ui.horizontal_wrapped(|ui| {
            ui.label("Places by fold:");
            for (i, c) in f.counts().iter().enumerate() {
                ui.colored_label(fold_color(i as i64), format!("{} {c}", i + 1));
            }
            let none = f.fold.iter().filter(|&&v| v < 0).count();
            if none > 0 {
                ui.weak(format!("(in none: {none})"));
            }
        });
    }
    for n in &s.notes {
        ui.weak(n);
    }
    if f.method == Method::Knndm && !f.gij.is_empty() {
        let ecdf = |name: &str, v: &[f64]| {
            let mut s: Vec<f64> = v.iter().copied().filter(|x| x.is_finite()).collect();
            s.sort_by(f64::total_cmp);
            let n = s.len().max(1) as f64;
            Line::new(name, PlotPoints::from_iter(s.iter().enumerate().map(|(i, &x)| [x, (i + 1) as f64 / n])))
        };
        Plot::new("ml knndm")
            .height(crate::fit::scaled(ui, 120.0))
            .legend(Legend::default().position(egui_plot::Corner::LeftTop))
            .allow_scroll(false)
            .x_axis_label(format!("distance to the nearest place ({})", s.unit))
            .show(ui, |p| {
                p.line(ecdf("map to the places", &f.gij));
                p.line(ecdf("place to the nearest other", &f.gj));
                p.line(ecdf("place to another fold's", &f.gjstar));
            });
    }
    ui.weak(format!("Written: {}", s.path));
}

#[cfg(test)]
mod tests {
    use super::*;
    use janus_cube::{BandSelection, open_cube};
    use janus_ml::forest::ForestHyper;
    use janus_ml::train::Rng;

    const W: usize = 60;
    const H: usize = 40;
    const T: usize = 12;

    fn class_at(x: usize) -> usize {
        x * 3 / W
    }

    /// A UTM series of 30 m pixels: three classes in strips, a field smooth over ~10 pixels on top, some clouds.
    fn cube(dir: &std::path::Path) -> Arc<CubeInfo> {
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir).unwrap();
        let drv = gdal::DriverManager::get_driver_by_name("GTiff").unwrap();
        let srs = gdal::spatial_ref::SpatialRef::from_epsg(32722).unwrap();
        let mut rng = Rng::new(5);
        for t in 0..T {
            let path = dir.join(format!("s_2021-{:02}-15.tif", t + 1));
            let mut ds = drv.create_with_band_type::<f32, _>(&path, W, H, 1).unwrap();
            ds.set_geo_transform(&[500000.0, 30.0, 0.0, 7500000.0, 0.0, -30.0]).unwrap();
            ds.set_spatial_ref(&srs).unwrap();
            let data: Vec<f32> = (0..W * H)
                .map(|p| {
                    let (x, y) = ((p % W) as f32, (p / W) as f32);
                    let season = (t as f32 / 12.0 * std::f32::consts::TAU).sin();
                    let level = [0.8, 0.5 + 0.3 * season, 0.2 + 0.05 * t as f32][class_at(p % W)];
                    let smooth = 0.1 * (x / 10.0).sin() * (y / 10.0).cos();
                    if rng.next_u64().is_multiple_of(12) { -9999.0 } else { level + smooth }
                })
                .collect();
            let mut band = ds.rasterband(1).unwrap();
            band.set_no_data_value(Some(-9999.0)).unwrap();
            band.write((0, 0), (W, H), &mut gdal::raster::Buffer::new((W, H), data)).unwrap();
        }
        Arc::new(open_cube(&[dir.to_string_lossy().into_owned()], &BandSelection::default()).unwrap())
    }

    /// Places of 2 x 2 pixels every 6 pixels (none across two strips), each of its strip's class.
    fn sites() -> (Vec<Site>, Vec<String>) {
        let classes: Vec<String> = ["forest", "crop", "regrowth"].iter().map(|s| s.to_string()).collect();
        let mut out = vec![];
        for gy in (1..H - 2).step_by(6) {
            for gx in (2..W - 2).step_by(6) {
                out.push(Site {
                    class: classes[class_at(gx)].clone(),
                    label: format!("Feature {}", out.len() + 1),
                    pin: None,
                    pixels: vec![(gx, gy), (gx + 1, gy), (gx, gy + 1), (gx + 1, gy + 1)],
                    mean: false,
                });
            }
        }
        (out, classes)
    }

    #[test]
    fn folds_of_a_series() {
        let dir = std::env::temp_dir().join("janus-folds-test");
        let info = cube(&dir.join("series"));
        let frame = Frame::of(&info);
        assert_eq!((frame.mx, frame.my, frame.metres), (30.0, 30.0, true));
        let (sites, classes) = sites();
        let n = sites.len();
        let inputs = vec![InputBand::shown(&info)];
        let notes = Mutex::new(vec![]);
        let note = |m: &str| notes.lock().unwrap().push(m.to_string());
        let make = |spec: FoldSpec| make_folds(&info, &inputs, &sites, &classes, &spec, None, 50.0, 1, 2, &note);
        // blocks of the autocorrelation's range: a few hundred metres
        let b = make(FoldSpec { method: Method::Blocks, k: 3, ..Default::default() }).unwrap();
        assert!(b.size > 60.0 && b.size < 1500.0, "range {}", b.size);
        assert!(notes.lock().unwrap().iter().any(|m| m.contains("median range")));
        assert_eq!(b.fold.len(), n);
        assert!(b.counts().iter().all(|&c| c > 0), "{:?}", b.counts());
        let k = make(FoldSpec { method: Method::Knndm, k: 4, ..Default::default() }).unwrap();
        assert!(k.w.is_finite() && k.counts().iter().all(|&c| c > 0));
        // clusters of the values: the strips' classes apart
        let c = make(FoldSpec { method: Method::Clusters, k: 3, ..Default::default() }).unwrap();
        assert!(c.fold.iter().all(|&f| f >= 0));
        let pure = (0..3).filter(|&f| {
            let of: std::collections::BTreeSet<&str> =
                (0..n).filter(|&i| c.fold[i] == f).map(|i| sites[i].class.as_str()).collect();
            of.len() == 1
        });
        assert_eq!(pure.count(), 3, "a cluster of two classes: {:?}", c.fold);
        let u = make(FoldSpec { method: Method::Buffer, size: 200.0, ..Default::default() }).unwrap();
        // neighbours 6 pixels (180 m) apart: 4 of them within 200 m inside the grid
        assert_eq!(u.exclude.iter().map(Vec::len).max(), Some(4));
        // a fold held out: whole places, every class still in the training
        let places: Vec<usize> = (0..n * 4).map(|i| i / 4).collect();
        let y: Vec<u32> =
            places.iter().map(|&p| classes.iter().position(|c| *c == sites[p].class).unwrap() as u32).collect();
        let cfg = TrainConfig::tempcnn();
        let (tr, va, how) = holdout(Some(&b), &places, &y, 3, &cfg);
        assert_eq!(how, "blocks");
        assert!(!va.is_empty() && tr.len() + va.len() == y.len());
        let fold_of = |i: usize| b.fold[places[i]];
        assert!(va.iter().all(|&i| fold_of(i) == fold_of(va[0])) && tr.iter().all(|&i| fold_of(i) != fold_of(va[0])));
        assert_eq!(tr.iter().map(|&i| y[i]).collect::<std::collections::BTreeSet<_>>().len(), 3);
        // the GeoPackage: the blocks and the places
        let path = dir.join("folds.gpkg");
        let feats = save_folds(&path, &b, &sites, &info).unwrap();
        assert_eq!(feats.len(), b.blocks.len() + n);
        let back = janus_geo::vectors::load_vector_geometry(&path.to_string_lossy(), 1_000_000).unwrap();
        assert_eq!((back[0].areas, back[0].points), (b.blocks.len(), n));
    }

    #[test]
    fn a_forest_cross_validated_on_blocks() {
        let dir = std::env::temp_dir().join("janus-folds-forest");
        let info = cube(&dir.join("series"));
        let (sites, classes) = sites();
        let arch = janus_ml::Arch::RandomForest(ForestHyper { trees: 40, ..Default::default() });
        let cfg = TrainConfig {
            folds: FoldSpec { method: Method::Blocks, k: 3, size: 400.0, ..Default::default() },
            ..TrainConfig::for_arch(&arch)
        };
        let model = dir.join("forest.janusmodel");
        let log = crate::zeit::JobLog { path: dir.join("forest.log"), t0: std::time::Instant::now() };
        let run = Arc::new(Mutex::new(crate::ml::TrainRun::default()));
        let h = crate::ml::train_job(
            &info,
            &[InputBand::shown(&info)],
            &sites,
            &classes,
            &[],
            &arch,
            &cfg,
            50.0,
            false,
            2,
            &model.to_string_lossy(),
            &log,
            &run,
            &|_, _| {},
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap();
        let s = &h.training["samples"];
        assert_eq!(s["split"], "blocks");
        assert_eq!(s["folds"]["k"], 3);
        // every sample predicted by a forest that did not see its fold
        let samples = s["per_class"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).sum::<u64>();
        assert_eq!(s["validation"].as_u64(), Some(samples));
        let oa = h.training["validation"]["overall_accuracy"].as_f64().unwrap();
        assert!(oa > 0.9, "cross-validated accuracy {oa}");
        assert!(h.applicability.as_ref().is_some_and(|a| a.profile.is_some()), "no error profile");
        assert!(dir.join("forest.folds.gpkg").exists());
        let text = std::fs::read_to_string(dir.join("forest.log")).unwrap();
        assert!(text.contains("cross-validated overall accuracy"), "{text}");
    }
}
