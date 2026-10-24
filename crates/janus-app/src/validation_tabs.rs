//! The Validation panel's tabs (src/app_validation.cpp): Design (the maps to stratify, buffer and limit, the strata,
//! the sample size and allocation, the draw; more points in the open project), Accuracy (translations, overall,
//! the error matrix, accuracies and areas, convergence, dates, change by year), Maps (one sample, many maps),
//! Interpreters (agreement, adjudication, copies merged) and Report (methods, the HTML report and its CSVs).

#![allow(clippy::needless_range_loop)] // parallel arrays indexed together, as the C++

use std::collections::BTreeMap;
use std::sync::Arc;

use egui::{Color32, Ui};
use egui_plot::{Bar, BarChart, Legend, Line, Plot, PlotPoints, Polygon};
use janus_geo::plane::Polygon as Poly;
use janus_geo::vectors::{Kind, Placer, VectorData, csv_field};
use janus_validation as val;
use val::{Alloc, AssessedMap, MapKind, MapRule, Progress, Project};

use crate::app::State;
use crate::exports::{ExportJob, start_export_job};
use crate::theme;
use crate::validation::{DrawResult, EVENT_LABELS, Job, binary_change, fmt_area, fmt_pm, html_esc};

/// The project as a sentence for a paper's methods.
fn methods_text(p: &Project, labelled: usize, interpreters: usize, confidence: f64) -> String {
    let strata: Vec<String> = p.strata.strata.iter().map(|s| format!("{} ({})", s.name, s.n)).collect();
    let maps: Vec<&str> = p.spec.maps.iter().map(|r| r.name.as_str()).collect();
    format!(
        "A stratified random sample of {} pixels was drawn from {} strata defined on {}{}{} \
         (points per stratum: {}; allocation: {}; seed {}), following the good practices of \
         Olofsson et al. (2014). The reference class of each sample pixel, and the date of any change, \
         were interpreted visually from its time series and images in Janus by {} interpreter(s); {} \
         points had a reference label. Accuracies (overall, user's and producer's) and error-adjusted \
         areas were estimated, with {}% confidence intervals, by the stratified estimators of \
         Stehman (2014), which equal those of Olofsson et al. (2014) when the strata are the map's classes.",
        p.points.len(),
        p.strata.strata.len(),
        maps.join(", "),
        if p.spec.buffer > 0 {
            format!(", with a buffer stratum of {} pixel(s) around change", p.spec.buffer)
        } else {
            String::new()
        },
        if p.spec.limit.is_empty() { "" } else { ", inside a study area" },
        strata.join(", "),
        p.design.get("alloc").and_then(|v| v.as_str()).unwrap_or("proportional"),
        p.seed,
        interpreters,
        labelled,
        (confidence * 100.0).round() as i32
    )
}

const REFERENCES: &str = "Olofsson, P., Foody, G. M., Herold, M., Stehman, S. V., Woodcock, C. E., Wulder, M. A. (2014). \
     Good practices for estimating area and assessing accuracy of land change. Remote Sensing of Environment, 148, \
     42-57.\nStehman, S. V. (2014). Estimating area and map accuracy for stratified random sampling when the strata \
     are different from the map classes. International Journal of Remote Sensing, 35(13), 4923-4939.";

/// A bar chart with error bars as SVG (the report's figures).
fn svg_bars(labels: &[String], series: &[Vec<f64>], errors: &[Vec<f64>], names: &[String], unit: &str) -> String {
    let (w, h, l, r, t, b) = (720, 300, 70, 10, 20, 60);
    let mut mx: f64 = 0.0;
    for (s, ser) in series.iter().enumerate() {
        for (i, v) in ser.iter().enumerate() {
            mx = mx.max(v + errors.get(s).and_then(|e| e.get(i)).copied().unwrap_or(0.0));
        }
    }
    if mx <= 0.0 {
        mx = 1.0;
    }
    let colors = ["#8a8f98", "#2a78d6", "#d64545"];
    let mut o = format!(
        "<svg viewBox=\"0 0 {w} {h}\" xmlns=\"http://www.w3.org/2000/svg\" font-family=\"sans-serif\" font-size=\"11\">"
    );
    let (pw, ph) = (f64::from(w - l - r), f64::from(h - t - b));
    for k in 0..=4 {
        let y = f64::from(t) + ph - ph * f64::from(k) / 4.0;
        o += &format!(
            "<line x1=\"{l}\" x2=\"{}\" y1=\"{y:.1}\" y2=\"{y:.1}\" stroke=\"#ccc\"/>\
             <text x=\"{}\" y=\"{:.1}\" text-anchor=\"end\" fill=\"#555\">{}</text>",
            w - r,
            l - 6,
            y + 4.0,
            fmt_area(mx * f64::from(k) / 4.0)
        );
    }
    let (n, ns) = (labels.len(), series.len());
    let gw = pw / n.max(1) as f64;
    let bw = gw * 0.7 / ns.max(1) as f64;
    let base = f64::from(t) + ph;
    for i in 0..n {
        for (s, ser) in series.iter().enumerate() {
            let v = ser.get(i).copied().unwrap_or(0.0);
            let x = f64::from(l) + gw * i as f64 + gw * 0.15 + bw * s as f64;
            let hh = ph * v / mx;
            o += &format!(
                "<rect x=\"{x:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{hh:.1}\" fill=\"{}\"/>",
                base - hh,
                bw * 0.92,
                colors[s % 3]
            );
            let e = errors.get(s).and_then(|e| e.get(i)).copied().unwrap_or(0.0);
            if e > 0.0 {
                let cx = x + bw * 0.46;
                let y0 = (base - ph * (v - e) / mx).min(base);
                let y1 = base - ph * (v + e) / mx;
                o += &format!(
                    "<line x1=\"{cx:.1}\" x2=\"{cx:.1}\" y1=\"{y0:.1}\" y2=\"{y1:.1}\" stroke=\"#222\"/>\
                     <line x1=\"{:.1}\" x2=\"{:.1}\" y1=\"{y1:.1}\" y2=\"{y1:.1}\" stroke=\"#222\"/>\
                     <line x1=\"{:.1}\" x2=\"{:.1}\" y1=\"{y0:.1}\" y2=\"{y0:.1}\" stroke=\"#222\"/>",
                    cx - 4.0,
                    cx + 4.0,
                    cx - 4.0,
                    cx + 4.0
                );
            }
        }
        o += &format!(
            "<text x=\"{:.1}\" y=\"{}\" text-anchor=\"middle\" fill=\"#333\">{}</text>",
            f64::from(l) + gw * i as f64 + gw / 2.0,
            t + ph as i32 + 16,
            html_esc(&labels[i])
        );
    }
    for (s, name) in names.iter().enumerate() {
        let s = s as i32;
        o += &format!(
            "<rect x=\"{}\" y=\"{}\" width=\"10\" height=\"10\" fill=\"{}\"/><text x=\"{}\" y=\"{}\">{}</text>",
            l + 10 + s * 130,
            h - 18,
            colors[s as usize % 3],
            l + 24 + s * 130,
            h - 9,
            html_esc(name)
        );
    }
    let mid = t + ph as i32 / 2;
    o += &format!(
        "<text x=\"12\" y=\"{mid}\" transform=\"rotate(-90 12 {mid})\" text-anchor=\"middle\">{}</text></svg>",
        html_esc(unit)
    );
    o
}

/// A map among the open ones, or a raster file: a button and its popup.
fn pick_source(ui: &mut Ui, src: &[crate::validation::Source], label: &str) -> Option<MapRule> {
    let mut got = None;
    ui.menu_button(label, |ui| {
        if src.is_empty() {
            ui.weak("No map open: run a tool, open a layer, or pick a file");
        }
        for s in src {
            if ui.button(&s.label).clicked() {
                got = Some(s.rule.clone());
                ui.close();
            }
        }
        ui.separator();
        if ui.button("A raster file...").clicked() {
            ui.close();
            if let Some(f) = rfd::FileDialog::new().pick_file() {
                got = Some(crate::validation::file_rule(&f.to_string_lossy()));
            }
        }
    });
    got
}

/// A file to write, the folder remembered.
fn ask_save(s: &mut State, title: &str, name: &str, filter: &str, ext: &str) -> Option<String> {
    let mut dlg = rfd::FileDialog::new().set_title(title).set_file_name(name).add_filter(filter, &[ext]);
    let folder = s.export_folder();
    if !folder.is_empty() {
        dlg = dlg.set_directory(folder);
    }
    let p = dlg.save_file()?.to_string_lossy().into_owned();
    s.remember_export_folder(&p);
    Some(p)
}

impl State {
    /// The strata's spec from the design's choices, the limit placed on the first map's grid.
    pub(crate) fn validation_spec(&self) -> Result<val::StrataSpec, String> {
        let v = &self.val;
        let mut spec = val::StrataSpec { maps: v.maps.clone(), buffer: v.buffer, limit: Vec::new() };
        if v.maps.is_empty() || v.limit == 0 {
            return Ok(spec);
        }
        let g = val::read_grid(&v.maps[0].path)?;
        if !g.has_gt {
            return Err("the map has no geotransform".into());
        }
        if v.limit == 2 {
            if let Some(w) = self.vector_index(v.limit_vector).map(|i| &self.vectors[i]) {
                let pl = Placer::new(&w.data, &g.wkt, Some(g.gt))?;
                for f in w.data.features.iter().filter(|f| f.kind == Kind::Area) {
                    spec.limit.extend(pl.placed(&f.parts));
                }
            }
        } else if let (Some(s), Some(r)) = (&self.session, self.series.roi_rect) {
            // the ROI (the active layer's pixels) on the map's grid, through their CRS
            let info = &s.info;
            let gt = info.geo_transform.ok_or("the active layer has no geotransform")?;
            let mut poly: Poly = self.series.roi_poly.clone();
            if poly.is_empty() {
                let [x0, y0, x1, y1] = r.map(|v| v as f64);
                poly = vec![vec![x0, y0, x1, y0, x1, y1, x0, y1]];
            }
            let mut poly = janus_geo::plane::densify(&poly, 8.0);
            janus_geo::plane::transform(&mut poly, |x, y| {
                Some((gt[0] + x * gt[1] + y * gt[2], gt[3] + x * gt[4] + y * gt[5]))
            });
            let tmp = VectorData { crs_wkt: info.crs_wkt.clone(), ..Default::default() };
            spec.limit = Placer::new(&tmp, &g.wkt, Some(g.gt))?.placed(&poly);
        }
        Ok(spec)
    }

    /// One pass over the maps: the strata raster (a preview in the app data), each stratum's pixels and area.
    pub(crate) fn validation_compute_strata(&mut self, spec: val::StrataSpec, ctx: &egui::Context) {
        let threads = self.session_settings().processing_threads();
        let v = &mut self.val;
        v.strata_prog = Arc::new(Progress::with_threads(threads));
        let dir = crate::settings::app_data_dir().join("validation");
        let _ = std::fs::create_dir_all(&dir);
        v.preview_count += 1;
        let out = dir.join(format!("strata_preview_{}.tif", v.preview_count)).to_string_lossy().into_owned();
        v.design_error.clear();
        let prog = v.strata_prog.clone();
        v.strata_job = Job::start(move || (val::compute_strata(&spec, &out, Some(&prog)), out), ctx);
    }

    /// The sample drawn into a new project at `path` (the strata raster copied beside it).
    pub(crate) fn validation_draw(&mut self, path: &str, spec: val::StrataSpec, ctx: &egui::Context) {
        let who = self.interpreter_name();
        let v = &mut self.val;
        let s = &v.strata.strata;
        let abs = std::path::absolute(path).unwrap_or_else(|_| path.into());
        let stem = abs.file_stem().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        let strata_path = abs.with_file_name(format!("{stem}_strata.tif")).to_string_lossy().into_owned();
        let alloc = Alloc::from_name(["proportional", "equal", "neyman", "manual"][v.alloc.min(3)]);
        let mut alloc_text = alloc.name().to_string();
        if alloc != Alloc::Manual && v.min_per > 0 {
            alloc_text += &format!(", at least {} per stratum", v.min_per);
        }
        let mut p = Project {
            path: abs.to_string_lossy().into_owned(),
            strata_path,
            id: val::new_project_id(),
            spec: spec.clone(),
            strata: v.strata.clone(),
            seed: v.seed.max(0) as u64,
            design: serde_json::json!({
                "alloc": alloc_text,
                "target_se": v.target_se,
                "min_per_stratum": v.min_per,
                "confidence": v.confidence,
                "created": val::now_iso(),
                "by": who,
            }),
            ..Default::default()
        };
        // the label set: a class map's classes, else land-change labels
        if spec.maps.len() == 1 && spec.maps[0].kind == MapKind::Classes {
            for t in s.iter().filter(|t| !t.buffer && t.cls != val::NONE) {
                p.label_set.push(spec.maps[0].class_name(t.cls));
            }
            p.label_set.push("Can't tell".into());
        } else {
            p.label_set = EVENT_LABELS.iter().map(|s| s.to_string()).collect();
        }
        for r in &spec.maps {
            let names: Vec<String> = if r.kind == MapKind::Classes {
                s.iter().filter(|t| t.cls != val::NONE).map(|t| r.class_name(t.cls)).collect()
            } else {
                Vec::new()
            };
            let mapping = val::default_mapping(r, &p.label_set, &names);
            p.maps.push(AssessedMap { rule: r.clone(), mapping });
        }
        let n: Vec<i32> = s.iter().map(|t| t.n).collect();
        let preview = v.strata_path.clone();
        v.draw_job = Job::start(
            move || {
                let mut j = DrawResult { p: Project::default(), error: String::new(), more: false, added: Vec::new() };
                if let Err(e) = std::fs::copy(&preview, &p.strata_path) {
                    j.error = format!("cannot copy the strata raster to {}: {e}", p.strata_path);
                }
                if j.error.is_empty() {
                    match val::draw_sample(&p.strata, &p.strata_path, &n, p.seed, &[]) {
                        Ok(pts) => p.points = pts,
                        Err(e) => j.error = e,
                    }
                }
                p.strata.row_counts.clear();
                if j.error.is_empty()
                    && let Err(e) = val::create_project(&p)
                {
                    j.error = e;
                }
                j.p = p;
                j
            },
            ctx,
        );
    }

    /// Design: the open project's strata and more points; a new design (maps, buffer and limit, strata, size and
    /// allocation, the draw).
    pub(crate) fn validation_design_ui(&mut self, ui: &mut Ui) {
        let busy = self.val.strata_job.busy() || self.val.draw_job.busy();
        let blind = self.val.blind && self.labels.project;
        if self.val.project.is_some() {
            egui::CollapsingHeader::new("The project's sample").default_open(true).show(ui, |ui| {
                self.validation_sample_ui(ui, busy, blind);
            });
            let open =
                egui::CollapsingHeader::new("A new design").show(ui, |ui| self.validation_new_design_ui(ui, busy));
            let _ = open;
            return;
        }
        self.validation_new_design_ui(ui, busy);
    }

    fn validation_sample_ui(&mut self, ui: &mut Ui, busy: bool, blind: bool) {
        let ctx = ui.ctx().clone();
        let v = &mut self.val;
        let p = v.project.as_ref().unwrap();
        let ns = p.strata.strata.len();
        let (mut labelled, mut total) = (vec![0; ns], vec![0; ns]);
        for (i, q) in p.points.iter().enumerate() {
            let Some(s) = p.strata.index_of_code(q.stratum) else { continue };
            total[s] += 1;
            labelled[s] += usize::from(val::resolve(&p.labels, i).0.is_some());
        }
        let name = |s: usize| if blind { format!("stratum {}", s + 1) } else { p.strata.strata[s].name.clone() };
        egui::Grid::new("pstrata").striped(true).num_columns(5).show(ui, |ui| {
            for h in ["stratum", "area", "weight", "points", "labelled"] {
                ui.strong(h);
            }
            ui.end_row();
            for (s, t) in p.strata.strata.iter().enumerate() {
                ui.label(name(s));
                ui.label(format!("{} {}", fmt_area(t.area), p.strata.unit));
                ui.label(format!("{:.4}", t.weight));
                ui.label(total[s].to_string());
                ui.label(labelled[s].to_string());
                ui.end_row();
            }
        });
        ui.weak(format!("Seed {}, {}", p.seed, p.design.get("alloc").and_then(|a| a.as_str()).unwrap_or("")));
        // more points in a stratum: drawn at random among its pixels not yet sampled (still a stratified random
        // sample)
        let names: Vec<String> = (0..ns).map(name).collect();
        v.more_stratum = v.more_stratum.min(ns.saturating_sub(1));
        let mut add = false;
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("morestratum")
                .width(220.0)
                .selected_text(names.get(v.more_stratum).cloned().unwrap_or_default())
                .show_ui(ui, |ui| {
                    for (s, n) in names.iter().enumerate() {
                        ui.selectable_value(&mut v.more_stratum, s, n);
                    }
                });
            ui.add(egui::DragValue::new(&mut v.more_n).range(1..=100000));
            add = ui
                .add_enabled(!busy && !names.is_empty(), egui::Button::new("Add points"))
                .on_hover_text(
                    "More points in this stratum, drawn at random among its pixels not sampled yet:\n\
                     the sample stays a stratified random sample (e.g. a rare class whose\n\
                     user's accuracy is still too uncertain)",
                )
                .clicked();
        });
        ui.add_space(4.0);
        if add {
            let mut n = vec![0; ns];
            n[v.more_stratum] = v.more_n;
            let mut copy = p.clone();
            copy.labels.clear();
            v.draw_job = Job::start(
                move || {
                    let mut j =
                        DrawResult { p: Project::default(), error: String::new(), more: true, added: Vec::new() };
                    let r = val::count_strata(&copy.strata_path.clone(), &mut copy.strata, None)
                        .and_then(|_| val::draw_sample(&copy.strata, &copy.strata_path, &n, copy.seed, &copy.points));
                    match r {
                        Err(e) => j.error = e,
                        Ok(added) => {
                            for q in &added {
                                if let Some(s) = copy.strata.index_of_code(q.stratum) {
                                    copy.strata.strata[s].n += 1;
                                }
                            }
                            if let Err(e) = val::append_points(&copy, &added) {
                                j.error = e;
                            }
                            j.added = added;
                        }
                    }
                    copy.strata.row_counts.clear();
                    j.p = copy;
                    j
                },
                &ctx,
            );
        }
    }

    fn validation_new_design_ui(&mut self, ui: &mut Ui, busy: bool) {
        ui.label(
            "Strata from one or more maps, a stratified random sample of their pixels, interpreted point by point: \
             accuracy and areas with confidence intervals (Olofsson et al. 2014).",
        );
        // --- the maps ---
        crate::panels::section(ui, "Maps to stratify");
        let src = self.validation_sources();
        let v = &mut self.val;
        v.bins_text.resize(v.maps.len(), String::new());
        v.change_text.resize(v.maps.len(), String::new());
        let mut remove = None;
        for k in 0..v.maps.len() {
            ui.push_id(k, |ui| {
                let r = &mut v.maps[k];
                ui.horizontal(|ui| {
                    if ui.small_button("x").clicked() {
                        remove = Some(k);
                    }
                    ui.label(&r.name).on_hover_text(format!("{} (band {})", r.path, r.band));
                });
                ui.indent("rule", |ui| {
                    ui.horizontal(|ui| {
                        let mut kind = r.kind;
                        egui::ComboBox::from_id_salt("kind")
                            .width(130.0)
                            .selected_text(kind.name())
                            .show_ui(ui, |ui| {
                                for k in [MapKind::Classes, MapKind::Intervals, MapKind::Events] {
                                    ui.selectable_value(&mut kind, k, k.name());
                                }
                            })
                            .response
                            .on_hover_text(
                                "classes: its values (a land-cover or class map)\n\
                             intervals: ranges of its values (e.g. a magnitude)\n\
                             events: a date of change (year of detection): > 0 change, else no change",
                            );
                        r.kind = kind;
                        if r.kind != MapKind::Classes {
                            if v.bins_text[k].is_empty() {
                                v.bins_text[k] = val::bins_text(&r.bins);
                            }
                            let hint = if r.kind == MapKind::Events {
                                "periods: e.g. 2000, 2010, 2021 (empty: change / no change)"
                            } else {
                                "edges: e.g. -1, -0.1, 1"
                            };
                            if ui
                                .add(
                                    egui::TextEdit::singleline(&mut v.bins_text[k])
                                        .hint_text(hint)
                                        .desired_width(f32::INFINITY),
                                )
                                .changed()
                                && let Ok(b) = val::parse_bins(&v.bins_text[k])
                            {
                                r.bins = b;
                            }
                        }
                    });
                    // which classes (or intervals) are change: for a buffer, the agreement of several maps, the
                    // expected errors
                    if r.kind == MapKind::Classes {
                        if !r.names.is_empty() {
                            ui.horizontal_wrapped(|ui| {
                                ui.weak("change:");
                                for (value, name) in r.names.clone() {
                                    let mut on = r.change.contains(&value);
                                    if ui.checkbox(&mut on, name).changed() {
                                        if on {
                                            r.change.insert(value);
                                        } else {
                                            r.change.remove(&value);
                                        }
                                    }
                                }
                            });
                        } else {
                            if v.change_text[k].is_empty() && !r.change.is_empty() {
                                v.change_text[k] =
                                    r.change.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(", ");
                            }
                            if ui
                                .add(
                                    egui::TextEdit::singleline(&mut v.change_text[k])
                                        .hint_text("values that are change, e.g. 3, 15 (for a buffer or several maps)")
                                        .desired_width(f32::INFINITY),
                                )
                                .changed()
                            {
                                r.change = v.change_text[k]
                                    .split([',', ';', ' '])
                                    .filter_map(|t| t.trim().parse::<f64>().ok())
                                    .map(|c| c as i32)
                                    .collect();
                            }
                        }
                    } else if r.kind == MapKind::Intervals && r.bins.len() >= 2 {
                        ui.horizontal_wrapped(|ui| {
                            ui.weak("change:");
                            for i in 1..r.bins.len() as i32 {
                                let mut on = r.change.contains(&i);
                                if ui.checkbox(&mut on, r.class_name(i)).changed() {
                                    if on {
                                        r.change.insert(i);
                                    } else {
                                        r.change.remove(&i);
                                    }
                                }
                            }
                        });
                    }
                });
            });
        }
        if let Some(k) = remove {
            v.maps.remove(k);
            v.bins_text.remove(k);
            v.change_text.remove(k);
        }
        if let Some(r) = pick_source(ui, &src, "Add a map...") {
            v.bins_text.push(val::bins_text(&r.bins));
            v.change_text.push(String::new());
            v.maps.push(r);
        }
        if v.maps.len() > 1 {
            ui.weak(format!(
                "Several maps: a stratum for each number of maps that call a pixel change (k of {}).",
                v.maps.len()
            ));
        }

        // --- buffer and limit ---
        crate::panels::section(ui, "Buffer and limit");
        ui.add(egui::Slider::new(&mut self.val.buffer, 0..=10).text("buffer (pixels)")).on_hover_text(
            "Pixels without change within this distance of a change form a stratum of their own:\n\
             omission errors (change the map missed) are most likely there, and a few points\n\
             in it make the area of change much more precise (Olofsson et al. 2020)",
        );
        let has_roi = self.series.roi_rect.is_some();
        let polys: Vec<(u32, String)> = self
            .vectors
            .iter()
            .filter(|w| w.data.areas > 0 && w.id != self.val.vector_id)
            .map(|w| (w.id, w.name.clone()))
            .collect();
        let v = &mut self.val;
        ui.horizontal_wrapped(|ui| {
            ui.radio_value(&mut v.limit, 0, "every pixel");
            ui.add_enabled_ui(has_roi, |ui| ui.radio_value(&mut v.limit, 1, "inside the ROI"));
            ui.add_enabled_ui(!polys.is_empty(), |ui| ui.radio_value(&mut v.limit, 2, "inside polygons of"));
            if v.limit == 2 && !polys.is_empty() {
                if !polys.iter().any(|(id, _)| *id == v.limit_vector) {
                    v.limit_vector = polys[0].0;
                }
                let cur = polys.iter().find(|(id, _)| *id == v.limit_vector).map(|p| p.1.clone()).unwrap_or_default();
                egui::ComboBox::from_id_salt("limitv").selected_text(cur).show_ui(ui, |ui| {
                    for (id, name) in &polys {
                        ui.selectable_value(&mut v.limit_vector, *id, name);
                    }
                });
            }
        });
        if (v.limit == 1 && !has_roi) || (v.limit == 2 && polys.is_empty()) {
            v.limit = 0;
        }

        // --- strata ---
        // what the strata depend on (cheap: the limit is placed on the map only when they are computed)
        let spec_key = format!(
            "{}|{}|{}|{}",
            serde_json::Value::Array(v.maps.iter().map(MapRule::to_json).collect()),
            v.buffer,
            v.limit,
            match v.limit {
                2 => v.limit_vector.to_string(),
                1 => self.series.roi_gen.to_string(),
                _ => String::new(),
            }
        );
        let stale = !v.strata_path.is_empty() && v.strata_for != spec_key;
        crate::panels::section(ui, "Strata");
        let mut compute = false;
        ui.horizontal(|ui| {
            let text = if v.strata_path.is_empty() { "Compute the strata" } else { "Compute the strata again" };
            compute = ui
                .add_enabled(!v.maps.is_empty() && !busy, egui::Button::new(text))
                .on_hover_text(
                    "One pass over the maps at full resolution: the strata raster (shown as a layer),\n\
                     each stratum's pixels and area",
                )
                .clicked();
            if v.strata_job.busy() {
                // Cancel at the row's end, the bar in what it leaves (never wider than the panel)
                crate::panels::progress_row(ui, egui::ProgressBar::new(v.strata_prog.done() as f32), |ui| {
                    if ui.small_button("Cancel").clicked() {
                        v.strata_prog.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                });
            } else if stale {
                ui.colored_label(theme::warning(), "the maps or the limit changed");
            }
        });
        if compute {
            match self.validation_spec() {
                Err(why) => self.val.design_error = format!("The limit cannot be placed on the map: {why}"),
                Ok(spec) => {
                    self.validation_compute_strata(spec, ui.ctx());
                    self.val.strata_for = spec_key.clone();
                }
            }
        }
        let v = &mut self.val;
        if !v.design_error.is_empty() {
            ui.colored_label(theme::error(), &v.design_error);
        }
        if v.strata_path.is_empty() || v.strata.strata.is_empty() {
            return;
        }

        // --- sample size and allocation ---
        ui.add(
            egui::Slider::new(&mut v.target_se, 0.005..=0.05)
                .fixed_decimals(3)
                .text("standard error of the overall accuracy"),
        );
        let eq13 = val::sample_size(&v.strata.strata, f64::from(v.target_se));
        ui.horizontal(|ui| {
            ui.checkbox(&mut v.n_auto, "n from it (eq. 13)");
            if v.n_auto {
                v.n = eq13;
            }
            ui.add_enabled(!v.n_auto, egui::DragValue::new(&mut v.n).range(1..=i32::MAX));
            ui.label("points");
        });
        v.n = v.n.max(1);
        ui.horizontal(|ui| {
            let names = ["proportional", "equal", "Neyman", "by hand"];
            egui::ComboBox::from_id_salt("alloc")
                .width(160.0)
                .selected_text(names[v.alloc.min(3)])
                .show_ui(ui, |ui| {
                    for (k, n) in names.iter().enumerate() {
                        ui.selectable_value(&mut v.alloc, k, *n);
                    }
                })
                .response
                .on_hover_text(
                    "proportional: to the strata's areas; equal: the same in each; Neyman: to area x the\n\
                     expected standard deviation; by hand: the n column. Rare strata (change) need\n\
                     50-100 points to estimate their accuracy (Olofsson et al. 2014)",
                );
            ui.label("allocation");
            if v.alloc != 3 {
                ui.add(egui::DragValue::new(&mut v.min_per).range(0..=i32::MAX));
                ui.label("at least, per stratum");
            }
        });
        let alloc = [Alloc::Proportional, Alloc::Equal, Alloc::Neyman, Alloc::Manual][v.alloc.min(3)];
        if alloc != Alloc::Manual {
            let n = val::allocate(&v.strata.strata, v.n, alloc, v.min_per);
            for (t, n) in v.strata.strata.iter_mut().zip(n) {
                t.n = n;
            }
        }
        let ex = val::expected_errors(&v.strata.strata, v.strata.total_area(), f64::from(v.confidence));
        let total: i32 = v.strata.strata.iter().map(|t| t.n).sum();
        let unit = v.strata.unit.clone();
        egui::Grid::new("strata").striped(true).num_columns(7).show(ui, |ui| {
            for h in ["stratum", "pixels", "area", "weight", "expected UA", "n", "SE of UA"] {
                ui.strong(h);
            }
            ui.end_row();
            for (s, t) in v.strata.strata.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(&t.name);
                    if t.change {
                        ui.colored_label(theme::warning(), "change");
                    }
                });
                ui.label(fmt_area(t.pixels as f64));
                ui.label(format!("{} {unit}", fmt_area(t.area)));
                ui.label(format!("{:.4}", t.weight));
                let mut ua = t.expected_ua as f32;
                if ui
                    .add(egui::DragValue::new(&mut ua).speed(0.005).range(0.5..=0.99).fixed_decimals(2))
                    .on_hover_text(
                        "The user's accuracy expected of this stratum: hard classes (change) 0.6-0.8,\n\
                         easy ones 0.9-0.95; it sets n (eq. 13) and the Neyman allocation",
                    )
                    .changed()
                {
                    t.expected_ua = f64::from(ua);
                }
                ui.add_enabled(alloc == Alloc::Manual, egui::DragValue::new(&mut t.n));
                t.n = i64::from(t.n).clamp(0, t.pixels) as i32;
                match ex.user_se.get(s).filter(|v| v.is_finite()) {
                    Some(se) => ui.label(format!("{se:.3}")),
                    None => ui.weak("-"),
                };
                ui.end_row();
            }
        });
        ui.label(format!("{total} points in all; area {} {unit}.", fmt_area(v.strata.total_area())));
        let expected = if ex.overall_se.is_finite() {
            let mut t = format!("Expected: overall accuracy SE {:.4}", ex.overall_se);
            if ex.change {
                t += &format!(
                    "; area of change {} +- {} {unit} ({}%)",
                    fmt_area(ex.change_area),
                    fmt_area(ex.change_ci),
                    (100.0 * v.confidence).round() as i32
                );
            }
            ui.label(t)
        } else {
            ui.weak("Expected errors: every stratum needs at least 2 points.")
        };
        expected.on_hover_text(
            "If each stratum's accuracy is as expected: the overall accuracy's standard error and\n\
             the interval of the area of change (a stratum's errors taken as change, or as no change)",
        );

        // --- draw ---
        let mut draw = false;
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut v.seed).range(0..=i32::MAX))
                .on_hover_text("The same strata, sizes and seed draw the same points");
            ui.label("seed");
            draw = ui
                .add_enabled(
                    !busy && total > 0 && !stale,
                    egui::Button::new("Draw the sample and create the project..."),
                )
                .clicked();
            if v.draw_job.busy() {
                ui.weak("drawing...");
            }
        });
        if draw {
            let stem = std::path::Path::new(&self.val.maps[0].path)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            if let Some(path) = ask_save(
                self,
                "Validation project (GeoPackage)",
                &format!("{stem}_validation.gpkg"),
                "GeoPackage",
                "gpkg",
            ) {
                match self.validation_spec() {
                    Ok(spec) => self.validation_draw(&path, spec, ui.ctx()),
                    Err(why) => self.val.design_error = format!("The limit cannot be placed on the map: {why}"),
                }
            }
        }
    }

    /// The selected map's assessment brought up to date.
    fn validation_assess(&mut self, k: usize, dates: bool) {
        let v = &mut self.val;
        let (Some(p), Some(d)) = (&v.project, v.data.get_mut(k)) else { return };
        if !d.ready() || (d.version == v.version && (!dates || d.tolerance == v.tolerance)) {
            return;
        }
        let m = &p.maps[k];
        let conf = f64::from(v.confidence);
        d.prep = val::prepare(p, m, &d.values, &d.mapped, conf, &[]);
        d.a = val::assess(&d.prep.input);
        d.change = binary_change(&m.rule, &d.prep);
        if dates && m.rule.kind == MapKind::Events {
            let st = d.prep.input.stratum.clone();
            let e = val::Estimator::new(&d.prep.input.stratum_area, &st, conf);
            let (mut my, mut ry) = (d.prep.map_year.clone(), d.prep.ref_year.clone());
            for i in 0..my.len() {
                if st[i].is_none() {
                    (my[i], ry[i]) = (0, 0);
                }
            }
            d.dates = val::date_agreement(&e, &my, &ry, f64::from(v.tolerance));
        }
        d.version = v.version;
        d.tolerance = if dates { v.tolerance } else { -1.0 };
    }

    /// Accuracy: the map assessed, its translations, the overall accuracy, the error matrix, accuracies and
    /// areas, convergence, dates, change by year.
    pub(crate) fn validation_accuracy_ui(&mut self, ui: &mut Ui) {
        let Some(p) = &self.val.project else {
            ui.label("Open a validation project (Validation > Open project...) or draw a sample (Design).");
            return;
        };
        if p.maps.is_empty() {
            ui.weak("No map to assess: add one in Maps.");
            return;
        }
        let nmaps = p.maps.len();
        let v = &mut self.val;
        v.selected = v.selected.min(nmaps - 1);
        ui.horizontal(|ui| {
            let p = v.project.as_ref().unwrap();
            // the slider and its value in 170 points: the row no wider than the panel
            ui.spacing_mut().slider_width = 60.0;
            egui::ComboBox::from_id_salt("map")
                .width((ui.available_width() - 190.0).max(80.0))
                .selected_text(&p.maps[v.selected].rule.name)
                .show_ui(ui, |ui| {
                    for k in 0..nmaps {
                        ui.selectable_value(&mut v.selected, k, &p.maps[k].rule.name);
                    }
                });
            let mut conf = (v.confidence * 100.0).round() as i32;
            if ui.add(egui::Slider::new(&mut conf, 80..=99).suffix("% intervals")).changed() {
                v.confidence = conf as f32 / 100.0;
                v.version += 1;
            }
        });
        let k = self.val.selected;
        {
            let d = &self.val.data[k];
            if !d.error.is_empty() {
                ui.colored_label(theme::error(), &d.error);
            }
            if !d.ready() {
                ui.weak("Reading the map at the points and its areas...");
                if d.mapped_busy() {
                    ui.add(egui::ProgressBar::new(d.prog.done() as f32));
                }
                return;
            }
        }
        self.validation_assess(k, true);
        let blind = self.val.blind && self.labels.project;
        let v = &mut self.val;
        let p = v.project.as_mut().unwrap();
        let d = &v.data[k];
        let (pp, a) = (&d.prep, &d.a);
        let pct = (v.confidence * 100.0).round() as i32;
        let unit = p.strata.unit.clone();

        // --- how far along ---
        let mut adjudicate = false;
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("{} point(s) used of {}", a.n, p.points.len()));
            ui.weak(format!("({} without a label)", pp.unlabelled));
            if pp.conflicts > 0 {
                ui.colored_label(theme::warning(), format!("{} disagreement(s)", pp.conflicts));
                adjudicate = ui.small_button("Adjudicate").clicked();
            }
        });
        if pp.unmapped > 0 {
            let ls: Vec<&str> = pp.unmapped_labels.iter().map(String::as_str).collect();
            ui.colored_label(
                theme::warning(),
                format!(
                    "{} point(s) with labels this map has no class for: {} (set below)",
                    pp.unmapped,
                    ls.join(", ")
                ),
            );
        }
        if pp.excluded > 0 {
            ui.weak(format!("{} point(s) left out by their label (can't tell...)", pp.excluded));
        }
        if pp.no_date > 0 {
            ui.colored_label(
                theme::warning(),
                format!("{} point(s) of change without a date (the period is unknown)", pp.no_date),
            );
        }
        if pp.off_map > 0 {
            ui.weak(format!("{} labelled point(s) where the map has no value", pp.off_map));
        }
        if !a.missing.is_empty() && a.n > 0 {
            let ms: Vec<String> = a
                .missing
                .iter()
                .map(|&s| if blind { (s + 1).to_string() } else { p.strata.strata[s].name.clone() })
                .collect();
            ui.colored_label(
                theme::warning(),
                format!(
                    "No point yet in: {} (the estimates cover {:.1}% of the area)",
                    ms.join(", "),
                    100.0 * a.covered
                ),
            );
        }

        // --- translations ---
        let m = &mut p.maps[k];
        let targets: Vec<String> = if m.rule.kind == MapKind::Events {
            vec![val::CHANGE.into(), val::NO_CHANGE.into()]
        } else {
            val::map_classes(&m.rule, &d.values, &d.mapped).into_iter().map(|c| m.rule.class_name(c)).collect()
        };
        let mut changed = false;
        egui::CollapsingHeader::new("Reference labels -> the map's classes")
            .id_salt("tr")
            .open(v.sections.map(|s| s & 1 == 1))
            .show(ui, |ui| {
                egui::Grid::new("trgrid").striped(true).num_columns(2).show(ui, |ui| {
                    for l in &p.label_set {
                        ui.label(l);
                        let cur = match m.mapping.get(l) {
                            None => "(not set)".to_string(),
                            Some(t) if t.is_empty() => "(left out)".to_string(),
                            Some(t) => t.clone(),
                        };
                        egui::ComboBox::from_id_salt(("t", l)).selected_text(cur).show_ui(ui, |ui| {
                            for t in &targets {
                                if ui.selectable_label(m.mapping.get(l) == Some(t), t).clicked() {
                                    m.mapping.insert(l.clone(), t.clone());
                                    changed = true;
                                }
                            }
                            if ui
                                .selectable_label(m.mapping.get(l).is_some_and(|t| t.is_empty()), "(left out)")
                                .clicked()
                            {
                                m.mapping.insert(l.clone(), String::new());
                                changed = true;
                            }
                        });
                        ui.end_row();
                    }
                });
                if m.rule.kind == MapKind::Events {
                    ui.weak("A point is change when any of its events is; with periods, by the date of the first.");
                }
            });
        if changed {
            v.meta_dirty = true;
            v.version += 1;
        }
        if a.n == 0 {
            if adjudicate {
                self.start_adjudication();
            }
            return;
        }

        // --- overall ---
        crate::panels::section(ui, "Overall");
        ui.label(format!("Overall accuracy: {:.3} +- {:.3} ({pct}%)", a.overall.est, a.overall.hi - a.overall.est));
        if d.change.defined {
            let c = &d.change;
            ui.label(format!(
                "Change: user's {}, producer's {}; area {} +- {} {unit} (mapped {})",
                fmt_pm(&c.users),
                fmt_pm(&c.producers),
                fmt_area(c.area.est),
                fmt_area(c.area.hi - c.area.est),
                fmt_area(c.mapped)
            ));
        }

        // --- error matrix ---
        let nc = a.classes.len();
        crate::panels::section(ui, "Error matrix");
        ui.horizontal_wrapped(|ui| {
            ui.radio_value(&mut v.matrix_mode, 0, "points");
            ui.radio_value(&mut v.matrix_mode, 1, "proportions of area");
            ui.weak("(rows: map, columns: reference; click a cell for its points)");
        });
        let mut cell = None;
        egui::ScrollArea::horizontal().id_salt("matrix").show(ui, |ui| {
            egui::Grid::new("matrixgrid").striped(true).num_columns(nc + 2).show(ui, |ui| {
                ui.strong("map \\ ref.");
                for c in &a.classes {
                    ui.strong(c);
                }
                ui.strong("total");
                ui.end_row();
                let mut col_total = vec![0.0; nc];
                for i in 0..nc {
                    ui.label(&a.classes[i]);
                    let mut row = 0.0;
                    for j in 0..nc {
                        let val = if v.matrix_mode == 1 { a.confusion[i][j] } else { f64::from(a.counts[i][j]) };
                        row += val;
                        col_total[j] += val;
                        let text = if v.matrix_mode == 1 { format!("{val:.4}") } else { a.counts[i][j].to_string() };
                        let fill = if i == j {
                            Color32::from_rgba_unmultiplied(80, 200, 120, 50)
                        } else {
                            Color32::TRANSPARENT
                        };
                        let r = egui::Frame::new().fill(fill).show(ui, |ui| ui.selectable_label(false, text)).inner;
                        if r.clicked() && a.counts[i][j] > 0 {
                            cell = Some((i, j));
                        }
                    }
                    ui.label(if v.matrix_mode == 1 { format!("{row:.4}") } else { (row as i64).to_string() });
                    ui.end_row();
                }
                ui.weak("total");
                for t in col_total {
                    ui.weak(if v.matrix_mode == 1 { format!("{t:.4}") } else { (t as i64).to_string() });
                }
                ui.end_row();
            });
        });

        // --- accuracies and areas ---
        crate::panels::section(ui, "Accuracy and areas");
        egui::Grid::new("acc").striped(true).num_columns(5).show(ui, |ui| {
            ui.strong("class");
            ui.strong("user's");
            ui.strong("producer's");
            ui.strong(format!("mapped ({unit})"));
            ui.strong(format!("adjusted ({unit})"));
            ui.end_row();
            for c in 0..nc {
                ui.label(&a.classes[c]);
                ui.label(fmt_pm(&a.users[c]));
                ui.label(fmt_pm(&a.producers[c]));
                ui.label(fmt_area(a.area[c].mapped));
                ui.label(format!("{} +- {}", fmt_area(a.area[c].est), fmt_area(a.area[c].ci)));
                ui.end_row();
            }
        });
        let fg = theme::on_plot(Color32::from_gray(200));
        // ImPlot's first two colours (its Deep colormap), as the C++'s bars: filled
        let (mapped_c, adj_c) =
            (theme::on_plot(Color32::from_rgb(76, 114, 176)), theme::on_plot(Color32::from_rgb(221, 132, 82)));
        let ticks: Vec<String> = a.classes.clone();
        Plot::new("areas")
            .height(200.0)
            .legend(Legend::default().position(egui_plot::Corner::RightTop))
            .y_axis_label(unit.clone())
            .x_axis_formatter(move |m, _| {
                let i = m.value.round();
                if (m.value - i).abs() < 1e-6 && i >= 0.0 {
                    ticks.get(i as usize).cloned().unwrap_or_default()
                } else {
                    String::new()
                }
            })
            .allow_drag(false)
            .allow_scroll(false)
            .show(ui, |pu| {
                let mapped: Vec<Bar> =
                    (0..nc).map(|c| Bar::new(c as f64 - 0.18, a.area[c].mapped).width(0.34).fill(mapped_c)).collect();
                let adj: Vec<Bar> = (0..nc)
                    .map(|c| Bar::new(c as f64 + 0.18, a.area[c].est.max(0.0)).width(0.34).fill(adj_c))
                    .collect();
                pu.bar_chart(BarChart::new("mapped", mapped).color(mapped_c));
                pu.bar_chart(BarChart::new("adjusted", adj).color(adj_c));
                for c in 0..nc {
                    let (x, e, ci) = (c as f64 + 0.18, a.area[c].est, a.area[c].ci);
                    if e.is_finite() && ci.is_finite() {
                        pu.line(Line::new("", PlotPoints::new(vec![[x, e - ci], [x, e + ci]])).color(fg).width(1.0));
                    }
                }
            });

        // --- the estimates as points came ---
        let conf = (v.confidence * 100.0).round() as u64;
        egui::CollapsingHeader::new("As the labels came (convergence)")
            .id_salt("conv")
            .open(v.sections.map(|s| s >> 1 & 1 == 1))
            .show(ui, |ui| {
                v.conv_class = v.conv_class.clamp(-1, nc as i32 - 1);
                let shown = if v.conv_class < 0 {
                    "overall accuracy".to_string()
                } else {
                    format!("area of {}", a.classes[v.conv_class as usize])
                };
                egui::ComboBox::from_id_salt("convc").width(220.0).selected_text(shown).show_ui(ui, |ui| {
                    ui.selectable_value(&mut v.conv_class, -1, "overall accuracy");
                    for c in 0..nc {
                        ui.selectable_value(&mut v.conv_class, c as i32, format!("area of {}", a.classes[c]));
                    }
                });
                let key = ((v.version.wrapping_mul(131) + v.selected as u64).wrapping_mul(131)
                    + (v.conv_class + 1) as u64)
                    .wrapping_mul(131)
                    + conf;
                if v.conv.key != key {
                    v.conv = crate::validation::Conv { key, ..Default::default() };
                    // the labelled points in the order of their labels' times, then the estimates at ~40 steps
                    let mut order: Vec<(String, usize)> = (0..p.points.len())
                        .filter_map(|i| val::resolve(&p.labels, i).0.map(|l| (l.updated.clone(), i)))
                        .collect();
                    order.sort();
                    let steps = order.len().min(40);
                    let mut use_ = vec![false; p.points.len()];
                    let mut next = 0;
                    for s in 1..=steps {
                        let upto = order.len() * s / steps;
                        while next < upto {
                            use_[order[next].1] = true;
                            next += 1;
                        }
                        let q = val::prepare(p, &p.maps[k], &d.values, &d.mapped, f64::from(v.confidence), &use_);
                        let b = val::assess(&q.input);
                        if b.n < 2 {
                            continue;
                        }
                        v.conv.n.push(upto as f64);
                        let (e, lo, hi) = if v.conv_class < 0 {
                            (b.overall.est, b.overall.lo, b.overall.hi)
                        } else {
                            let name = &a.classes[v.conv_class as usize];
                            match b.classes.iter().position(|c| c == name) {
                                Some(c) => (b.area[c].est, b.area[c].lo, b.area[c].hi),
                                None => (f64::NAN, f64::NAN, f64::NAN),
                            }
                        };
                        v.conv.est.push(e);
                        v.conv.lo.push(lo);
                        v.conv.hi.push(hi);
                    }
                }
                let col = theme::on_plot(Color32::from_rgb(77, 153, 255));
                Plot::new("convplot")
                    .height(180.0)
                    .x_axis_label("labelled points")
                    .y_axis_label(if v.conv_class < 0 { "overall accuracy".to_string() } else { unit.clone() })
                    .allow_drag(false)
                    .allow_scroll(false)
                    .show(ui, |pu| {
                        let c = &v.conv;
                        let mut band: Vec<[f64; 2]> = c.n.iter().zip(&c.lo).map(|(x, y)| [*x, *y]).collect();
                        band.extend(c.n.iter().zip(&c.hi).rev().map(|(x, y)| [*x, *y]));
                        if band.len() >= 4 {
                            pu.polygon(
                                Polygon::new("interval", PlotPoints::new(band))
                                    .fill_color(col.gamma_multiply(0.25))
                                    .stroke(egui::Stroke::NONE),
                            );
                        }
                        pu.line(
                            Line::new(
                                "estimate",
                                PlotPoints::new(c.n.iter().zip(&c.est).map(|(x, y)| [*x, *y]).collect()),
                            )
                            .color(col)
                            .width(1.5),
                        );
                    });
                ui.weak("The interval narrows as points are labelled: label until it is narrow enough.");
            });

        // --- dates ---
        if p.maps[k].rule.kind == MapKind::Events {
            egui::CollapsingHeader::new("Dates of change")
                .id_salt("dates")
                .open(v.sections.map(|s| s >> 2 & 1 == 1))
                .show(ui, |ui| {
                    ui.add(egui::Slider::new(&mut v.tolerance, 0.0..=5.0).step_by(1.0).text("tolerance (years)"));
                    let g = &d.dates;
                    if g.n == 0 {
                        ui.weak("No point that both the map and the reference call change, with dates.");
                    } else {
                        ui.label(format!(
                            "Within +-{:.0} year(s): {}; mean difference {:.2} years ({} points)",
                            g.tolerance,
                            fmt_pm(&g.within),
                            g.mean_diff.est,
                            g.n
                        ));
                        Plot::new("ddiff")
                            .height(150.0)
                            .x_axis_label("map year - reference year")
                            .y_axis_label("points")
                            .allow_drag(false)
                            .allow_scroll(false)
                            .show(ui, |pu| {
                                let bars: Vec<Bar> = g
                                    .diffs
                                    .iter()
                                    .map(|(&k, &n)| Bar::new(f64::from(k), f64::from(n)).width(0.8).fill(mapped_c))
                                    .collect();
                                pu.bar_chart(BarChart::new("points", bars).color(mapped_c));
                            });
                    }
                });
        }
        // --- area of change by year (the reference's dates) ---
        let any_change = p.maps[k].mapping.values().any(|t| t == val::CHANGE);
        if p.maps[k].rule.kind == MapKind::Events && any_change {
            egui::CollapsingHeader::new("Area of change by year")
                .id_salt("byyear")
                .open(v.sections.map(|s| s >> 3 & 1 == 1))
                .show(ui, |ui| {
                    let key = (v.version.wrapping_mul(131) + v.selected as u64).wrapping_mul(131) + conf;
                    if v.by_year.0 != key {
                        v.by_year = (key, val::change_by_year(p, &p.maps[k], f64::from(v.confidence)));
                    }
                    if v.by_year.1.is_empty() {
                        ui.weak("No dated change in the reference yet.");
                    } else {
                        Plot::new("byyearplot")
                            .height(190.0)
                            .x_axis_label("year")
                            .y_axis_label(unit.clone())
                            .x_axis_formatter(|m, _| format!("{:.0}", m.value))
                            .allow_drag(false)
                            .allow_scroll(false)
                            .show(ui, |pu| {
                                let bars: Vec<Bar> = v
                                    .by_year
                                    .1
                                    .iter()
                                    .map(|(y, ar)| Bar::new(f64::from(*y), ar.est).width(0.7).fill(mapped_c))
                                    .collect();
                                pu.bar_chart(BarChart::new("area", bars).color(mapped_c));
                                for (y, ar) in &v.by_year.1 {
                                    pu.line(
                                        Line::new(
                                            "",
                                            PlotPoints::new(vec![
                                                [f64::from(*y), ar.est - ar.ci],
                                                [f64::from(*y), ar.est + ar.ci],
                                            ]),
                                        )
                                        .color(fg)
                                        .width(1.0),
                                    );
                                }
                            });
                        ui.weak(format!(
                            "From the reference alone: the area whose first change (as this map's translation\n\
                         reads the labels) falls in each year, +- its {pct}% interval."
                        ));
                    }
                });
        }
        if adjudicate {
            self.start_adjudication();
        }
        if let Some((i, j)) = cell {
            let a = &self.val.data[k].a;
            self.labels.filter_name = format!("map {}, reference {}", a.classes[i], a.classes[j]);
            self.labels.filter = a.cells[i][j].clone();
            if self.labels.vector != Some(self.val.vector_id) {
                self.start_interpretation(self.val.vector_id);
            }
            if let Some(&f) = self.labels.filter.first() {
                self.interp_go(f);
            }
            self.labels.show = true;
        }
    }

    /// Maps: every map assessed with the sample, side by side; another one added; how the selected one is read.
    pub(crate) fn validation_maps_ui(&mut self, ui: &mut Ui) {
        if self.val.project.is_none() {
            ui.label("Open a project first: every map added here is assessed with its sample and its labels.");
            return;
        }
        ui.label(
            "One sample, many maps: each map is read at the points and translated from the same reference labels; \
             the estimators weigh the points by the project's strata (Stehman 2014), so maps other than the one \
             stratified are assessed without bias.",
        );
        let nmaps = self.val.project.as_ref().unwrap().maps.len();
        for k in 0..nmaps {
            self.validation_assess(k, false);
        }
        let v = &mut self.val;
        let p = v.project.as_ref().unwrap();
        let unit = p.strata.unit.clone();
        let mut remove = None;
        egui::ScrollArea::horizontal().id_salt("maps").show(ui, |ui| {
            egui::Grid::new("mapsgrid").striped(true).num_columns(8).show(ui, |ui| {
                for h in ["map", "points", "overall", "change: user's", "producer's"] {
                    ui.strong(h);
                }
                ui.strong(format!("mapped ({unit})"));
                ui.strong(format!("adjusted ({unit})"));
                ui.label("");
                ui.end_row();
                for k in 0..nmaps {
                    let d = &v.data[k];
                    let r = &p.maps[k].rule;
                    if ui
                        .selectable_label(k == v.selected, &r.name)
                        .on_hover_text(format!("{} ({})", r.path, r.kind.name()))
                        .clicked()
                    {
                        v.selected = k;
                        v.select_tab = Some(1);
                    }
                    if !d.ready() {
                        if d.error.is_empty() {
                            ui.weak("reading...");
                        } else {
                            ui.weak("error").on_hover_text(&d.error);
                        }
                        for _ in 0..5 {
                            ui.label("");
                        }
                    } else {
                        let c = &d.change;
                        ui.label(d.a.n.to_string());
                        ui.label(fmt_pm(&d.a.overall));
                        ui.label(if c.defined { fmt_pm(&c.users) } else { "-".into() });
                        ui.label(if c.defined { fmt_pm(&c.producers) } else { "-".into() });
                        ui.label(if c.defined { fmt_area(c.mapped) } else { "-".into() });
                        ui.label(if c.defined {
                            format!("{} +- {}", fmt_area(c.area.est), fmt_area(c.area.hi - c.area.est))
                        } else {
                            "-".into()
                        });
                    }
                    if ui.small_button("x").clicked() {
                        remove = Some(k);
                    }
                    ui.end_row();
                }
            });
        });
        if let Some(k) = remove.filter(|_| nmaps > 1) {
            v.project.as_mut().unwrap().maps.remove(k);
            v.data.remove(k);
            v.selected = 0;
            v.meta_dirty = true;
            v.version += 1;
        }
        ui.weak("Change: the classes each map calls change (a map of events: any date).");
        let src = self.validation_sources();
        let picked = pick_source(ui, &src, "Assess another map...");
        if let Some(rule) = picked {
            let p = self.val.project.as_mut().unwrap();
            p.maps.push(AssessedMap { rule, mapping: BTreeMap::new() });
            let k = p.maps.len() - 1;
            self.validation_add_map_data(k, ui.ctx());
            self.val.meta_dirty = true;
            self.val.version += 1;
        }
        // the rule of the selected map (e.g. periods of a map of events)
        let v = &mut self.val;
        let sel = v.selected;
        let Some(r) = v.project.as_mut().unwrap().maps.get_mut(sel).map(|m| &mut m.rule) else { return };
        crate::panels::section(ui, &format!("How {} is read", r.name));
        let mut changed = false;
        ui.horizontal(|ui| {
            let mut kind = r.kind;
            egui::ComboBox::from_id_salt("kindm").width(130.0).selected_text(kind.name()).show_ui(ui, |ui| {
                for k in [MapKind::Classes, MapKind::Intervals, MapKind::Events] {
                    ui.selectable_value(&mut kind, k, k.name());
                }
            });
            if kind != r.kind {
                r.kind = kind;
                changed = true;
            }
            if r.kind != MapKind::Classes {
                if v.bins_m.0 != Some(sel) {
                    v.bins_m = (Some(sel), val::bins_text(&r.bins));
                }
                let hint = if r.kind == MapKind::Events { "periods (empty: change / no change)" } else { "edges" };
                ui.add(
                    egui::TextEdit::singleline(&mut v.bins_m.1)
                        .hint_text(hint)
                        .desired_width(ui.available_width() - 60.0),
                );
                if ui.button("Apply").clicked() {
                    match val::parse_bins(&v.bins_m.1) {
                        Ok(b) => {
                            r.bins = b;
                            changed = true;
                        }
                        Err(e) => v.message = e,
                    }
                }
            }
        });
        if changed {
            v.project.as_mut().unwrap().maps[sel].mapping.clear();
            self.validation_add_map_data(sel, ui.ctx());
            self.val.meta_dirty = true;
            self.val.version += 1;
        }
    }

    /// Interpreters: who labelled what, their agreement, the disagreements, copies for others and their merge.
    pub(crate) fn validation_interpreters_ui(&mut self, ui: &mut Ui) {
        let Some(p) = &self.val.project else {
            ui.label("Open a project first.");
            return;
        };
        let me = self.interpreter_name();
        let mut settings = false;
        ui.horizontal(|ui| {
            ui.label(format!("You label as \"{me}\""));
            settings = ui.small_button("Change...").clicked();
        });
        let who: Vec<String> = p.labels.keys().filter(|w| *w != val::ADJUDICATED).cloned().collect();
        egui::Grid::new("who").striped(true).num_columns(3).show(ui, |ui| {
            ui.strong("interpreter");
            ui.strong("points");
            ui.strong("last label");
            ui.end_row();
            for (w, l) in &p.labels {
                let given: Vec<&val::Label> = l.iter().filter(|x| !x.label.is_empty()).collect();
                let last = given.iter().map(|x| x.updated.as_str()).max().unwrap_or("");
                ui.label(w);
                ui.label(given.len().to_string());
                ui.label(last);
                ui.end_row();
            }
        });
        // agreement of every pair, on the points both labelled
        if who.len() >= 2 {
            crate::panels::section(ui, "Agreement");
            egui::Grid::new("kappa").striped(true).num_columns(5).show(ui, |ui| {
                for h in ["interpreters", "both", "agree", "kappa", "dates within 1 yr"] {
                    ui.strong(h);
                }
                ui.end_row();
                for i in 0..who.len() {
                    for j in i + 1..who.len() {
                        let (la, lb) = (&p.labels[&who[i]], &p.labels[&who[j]]);
                        let lab = |l: &Vec<val::Label>, k: usize| l.get(k).map_or(String::new(), |x| x.label.clone());
                        let a: Vec<String> = (0..p.points.len()).map(|k| lab(la, k)).collect();
                        let b: Vec<String> = (0..p.points.len()).map(|k| lab(lb, k)).collect();
                        let (mut dates, mut ok) = (0, 0);
                        for k in 0..p.points.len() {
                            let ya = la.get(k).map_or(0, |x| val::year_of(&x.date));
                            let yb = lb.get(k).map_or(0, |x| val::year_of(&x.date));
                            if ya != 0 && yb != 0 {
                                dates += 1;
                                ok += i32::from((ya - yb).abs() <= 1);
                            }
                        }
                        let g = val::cohen(&a, &b);
                        ui.label(format!("{} / {}", who[i], who[j]));
                        ui.label(g.n.to_string());
                        ui.label(if g.n > 0 { format!("{:.1}%", 100.0 * g.observed) } else { String::new() });
                        ui.label(if g.n > 0 { format!("{:.3}", g.kappa) } else { String::new() });
                        ui.label(if dates > 0 { format!("{ok} of {dates}") } else { String::new() });
                        ui.end_row();
                    }
                }
            });
            ui.weak("Cohen's kappa: the agreement beyond chance (points, not areas).");
        }
        let conflicts = self.val.conflicts();
        crate::panels::section(ui, "Disagreements");
        let mut adjudicate = false;
        ui.horizontal(|ui| {
            ui.label(format!("{conflicts} point(s) where interpreters disagree"));
            adjudicate = ui
                .add_enabled(conflicts > 0 && self.session.is_some(), egui::Button::new("Adjudicate..."))
                .on_hover_text(format!(
                    "Goes through them; the label you give settles each (\"{}\"), the others' labels shown",
                    val::ADJUDICATED
                ))
                .clicked();
        });
        crate::panels::section(ui, "Several interpreters");
        ui.label(
            "Each interpreter can label a copy of the project (the GeoPackage and its strata raster); their labels \
             are then merged here, by point.",
        );
        let (mut copy, mut merge) = (false, false);
        ui.horizontal(|ui| {
            copy = ui.button("Save a copy for an interpreter...").clicked();
            merge = ui.button("Merge labels from a copy...").clicked();
        });
        if settings {
            self.settings.show = true;
        }
        if adjudicate {
            self.start_adjudication();
        }
        if copy {
            let p = self.val.project.as_ref().unwrap();
            let stem =
                std::path::Path::new(&p.path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            if let Some(path) =
                ask_save(self, "A copy of the project", &format!("{stem}_copy.gpkg"), "GeoPackage", "gpkg")
            {
                self.validation_save_copy(&path);
            }
        }
        if merge && let Some(f) = rfd::FileDialog::new().add_filter("GeoPackage", &["gpkg"]).pick_file() {
            let (_, msg) = self.validation_merge(&f.to_string_lossy());
            self.val.message = msg;
        }
    }

    /// The project written to `path` for another interpreter, with its strata raster at the same relative place.
    pub(crate) fn validation_save_copy(&mut self, path: &str) {
        self.validation_save_labels();
        // every write done first
        loop {
            if self.val.writer.busy() {
                let _ = self.val.writer.wait();
            }
            let Some(t) = self.val.tasks.pop_front() else { break };
            let e = t();
            if !e.is_empty() {
                self.val.write_error = e;
            }
        }
        let p = self.val.project.as_ref().unwrap();
        let mut r = std::fs::copy(&p.path, path).map(|_| ());
        let base = std::path::Path::new(&p.path).parent().unwrap_or(std::path::Path::new(""));
        if r.is_ok()
            && let Ok(rel) = std::path::Path::new(&p.strata_path).strip_prefix(base)
        {
            let to = std::path::Path::new(path).parent().unwrap_or(std::path::Path::new("")).join(rel);
            if to != std::path::Path::new(&p.strata_path) {
                if let Some(d) = to.parent() {
                    let _ = std::fs::create_dir_all(d);
                }
                r = std::fs::copy(&p.strata_path, &to).map(|_| ());
            }
        }
        self.val.message = match r {
            Ok(()) => format!("Copied to {path} (with its strata raster)."),
            Err(e) => format!("The copy failed: {e}"),
        };
    }

    /// Report: the export, Copy Python, the methods and references.
    pub(crate) fn validation_report_ui(&mut self, ui: &mut Ui) {
        let Some(p) = &self.val.project else {
            ui.label("Open a project first.");
            return;
        };
        let labelled = (0..p.points.len()).filter(|&i| val::resolve(&p.labels, i).0.is_some()).count();
        let interpreters = p.labels.keys().filter(|w| *w != val::ADJUDICATED).count();
        let text = methods_text(p, labelled, interpreters, f64::from(self.val.confidence));
        let (mut export, mut python) = (false, false);
        ui.horizontal(|ui| {
            export = ui
                .button("Export the report...")
                .on_hover_text(
                    "report.html (tables, figures, methods, references) and a CSV of each table, in a folder",
                )
                .clicked();
            python = ui
                .button("Copy Python")
                .on_hover_text(
                    "zeit.accuracy on the project's GeoPackage and rasters, for the map selected in Accuracy",
                )
                .clicked();
        });
        crate::panels::section(ui, "Methods");
        ui.label(&text);
        if ui.small_button("Copy").clicked() {
            ui.ctx().copy_text(format!("{text}\n\n{REFERENCES}"));
        }
        crate::panels::section(ui, "References");
        ui.weak(REFERENCES);
        if python && !p.maps.is_empty() {
            let k = self.val.selected.min(p.maps.len() - 1);
            ui.ctx().copy_text(val::python_snippet(p, &p.maps[k], f64::from(self.val.confidence)));
            self.val.message = format!(
                "Python for {} copied: zeit.accuracy on the project's files gives the same numbers.",
                p.maps[k].rule.name
            );
        }
        if export && let Some(dir) = rfd::FileDialog::new().pick_folder() {
            self.export_validation_report(&dir.to_string_lossy(), ui.ctx());
        }
    }

    /// report.html (tables, figures, methods, references) and a CSV of each table, written in the background.
    pub fn export_validation_report(&mut self, folder: &str, ctx: &egui::Context) -> Option<Arc<ExportJob>> {
        let n = self.val.project.as_ref()?.maps.len();
        for k in 0..n {
            self.validation_assess(k, false);
        }
        let v = &self.val;
        let p = v.project.as_ref()?;
        let conf = f64::from(v.confidence);
        // everything the report needs, computed now (cheap), written in the background
        struct MapOut {
            name: String,
            a: val::Assessment,
            change: crate::validation::BinaryChange,
            by_year: Vec<(i32, val::ClassArea)>,
            ready: bool,
        }
        let maps: Vec<MapOut> = (0..n)
            .map(|k| {
                let d = &v.data[k];
                let m = &p.maps[k];
                MapOut {
                    name: m.rule.name.clone(),
                    a: if d.ready() { d.a.clone() } else { Default::default() },
                    change: if d.ready() { d.change.clone() } else { Default::default() },
                    by_year: if d.ready() && m.rule.kind == MapKind::Events {
                        val::change_by_year(p, m, conf)
                    } else {
                        Vec::new()
                    },
                    ready: d.ready(),
                }
            })
            .collect();
        let labelled = (0..p.points.len()).filter(|&i| val::resolve(&p.labels, i).0.is_some()).count();
        let who: Vec<String> = p.labels.keys().filter(|w| *w != val::ADJUDICATED).cloned().collect();
        let methods = methods_text(p, labelled, who.len(), conf);
        let proj = p.clone();
        let dir = std::path::PathBuf::from(folder);
        let html_path = dir.join("report.html").to_string_lossy().into_owned();
        let job = start_export_job("Validation report".into(), html_path, ctx, move |j| {
            let p = &proj;
            let pct = (conf * 100.0).round() as i32;
            let write = |name: &str, text: &str| -> Result<(), String> {
                let f = dir.join(name);
                std::fs::write(&f, text).map_err(|e| format!("cannot write {}: {e}", f.display()))
            };
            // design
            let mut csv = format!("code,stratum,pixels,area_{},weight,expected_ua,n\n", p.strata.unit);
            for s in &p.strata.strata {
                csv += &format!(
                    "{},{},{},{:.6},{:.8},{:.3},{}\n",
                    s.code,
                    csv_field(&s.name),
                    s.pixels,
                    s.area,
                    s.weight,
                    s.expected_ua,
                    s.n
                );
            }
            write("design.csv", &csv)?;
            let mut h = String::from(
                "<!doctype html><html><head><meta charset=\"utf-8\"><title>Validation report</title><style>\
                 body{font-family:system-ui,sans-serif;max-width:980px;margin:24px auto;padding:0 16px;color:#222}\
                 table{border-collapse:collapse;margin:8px 0 18px}td,th{border:1px solid #ccc;padding:3px 8px;text-align:right}\
                 th{background:#f3f3f3}td:first-child,th:first-child{text-align:left}.diag{background:#e6f5ea}\
                 h2{margin-top:32px;border-bottom:1px solid #ddd}p.small{color:#666;font-size:90%}</style></head><body>",
            );
            let file =
                std::path::Path::new(&p.path).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
            h += &format!(
                "<h1>Validation report</h1><p class=\"small\">{} &middot; {} &middot; Janus</p>",
                html_esc(&file),
                html_esc(&val::now_iso())
            );
            h += &format!("<h2>Methods</h2><p>{}</p>", html_esc(&methods));
            h += &format!(
                "<h2>Sampling design</h2><table><tr><th>stratum</th><th>pixels</th><th>area ({})</th><th>weight</th>\
                 <th>expected UA</th><th>points</th></tr>",
                html_esc(&p.strata.unit)
            );
            for s in &p.strata.strata {
                h += &format!(
                    "<tr><td>{}</td><td>{}</td><td>{}</td><td>{:.4}</td><td>{:.2}</td><td>{}</td></tr>",
                    html_esc(&s.name),
                    fmt_area(s.pixels as f64),
                    fmt_area(s.area),
                    s.weight,
                    s.expected_ua,
                    s.n
                );
            }
            h += "</table>";
            // comparison of the maps
            let mut cmp = String::from(
                "map,points,overall,overall_ci,change_users,change_users_ci,change_producers,change_producers_ci,\
                 change_mapped,change_adjusted,change_adjusted_ci\n",
            );
            h += &format!(
                "<h2>Maps</h2><table><tr><th>map</th><th>points</th><th>overall</th><th>change: user's</th>\
                 <th>producer's</th><th>mapped</th><th>adjusted (&plusmn;{pct}%)</th></tr>"
            );
            for m in maps.iter().filter(|m| m.ready) {
                let (a, c) = (&m.a, &m.change);
                cmp += &format!(
                    "{},{},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{:.4},{:.4},{:.4}\n",
                    csv_field(&m.name),
                    a.n,
                    a.overall.est,
                    a.overall.hi - a.overall.est,
                    c.users.est,
                    c.users.hi - c.users.est,
                    c.producers.est,
                    c.producers.hi - c.producers.est,
                    c.mapped,
                    c.area.est,
                    c.area.hi - c.area.est
                );
                let dash = || "-".to_string();
                h += &format!(
                    "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                    html_esc(&m.name),
                    a.n,
                    fmt_pm(&a.overall),
                    if c.defined { fmt_pm(&c.users) } else { dash() },
                    if c.defined { fmt_pm(&c.producers) } else { dash() },
                    if c.defined { fmt_area(c.mapped) } else { dash() },
                    if c.defined {
                        format!("{} &plusmn; {}", fmt_area(c.area.est), fmt_area(c.area.hi - c.area.est))
                    } else {
                        dash()
                    }
                );
            }
            h += "</table>";
            write("maps.csv", &cmp)?;
            j.set_progress(0.3);
            // each map: error matrix, accuracies and areas
            for (k, m) in maps.iter().enumerate() {
                if !m.ready {
                    continue;
                }
                let a = &m.a;
                let nc = a.classes.len();
                let tag = format!("map{}", k + 1);
                h += &format!(
                    "<h2>{}</h2><p>Overall accuracy {} ({pct}% interval), {} points.</p>",
                    html_esc(&m.name),
                    fmt_pm(&a.overall),
                    a.n
                );
                let mut mc = String::from("map\\reference");
                for c in &a.classes {
                    mc += &format!(",{}", csv_field(c));
                }
                mc.push('\n');
                let mut mp = mc.clone();
                h += "<table><tr><th>map \\ reference</th>";
                for c in &a.classes {
                    h += &format!("<th>{}</th>", html_esc(c));
                }
                h += "</tr>";
                for i in 0..nc {
                    mc += &csv_field(&a.classes[i]);
                    mp += &csv_field(&a.classes[i]);
                    h += &format!("<tr><td>{}</td>", html_esc(&a.classes[i]));
                    for jj in 0..nc {
                        mp += &format!(",{:.8}", a.confusion[i][jj]);
                        mc += &format!(",{}", a.counts[i][jj]);
                        h += &format!(
                            "<td{}>{} <small>({:.4})</small></td>",
                            if i == jj { " class=\"diag\"" } else { "" },
                            a.counts[i][jj],
                            a.confusion[i][jj]
                        );
                    }
                    mc.push('\n');
                    mp.push('\n');
                    h += "</tr>";
                }
                h += "</table><p class=\"small\">Points (and proportions of area). Rows: the map; columns: the reference.</p>";
                write(&format!("{tag}_matrix_points.csv"), &mc)?;
                write(&format!("{tag}_matrix_area.csv"), &mp)?;
                let mut ac = String::from(
                    "class,users,users_se,users_ci,producers,producers_se,producers_ci,mapped,adjusted,adjusted_se,adjusted_ci\n",
                );
                h += &format!(
                    "<table><tr><th>class</th><th>user's</th><th>producer's</th><th>mapped ({u})</th><th>adjusted ({u})</th></tr>",
                    u = html_esc(&p.strata.unit)
                );
                let (mut labels, mut series, mut errs) =
                    (Vec::new(), vec![Vec::new(), Vec::new()], vec![Vec::new(), Vec::new()]);
                for c in 0..nc {
                    let (u, pr, ar) = (&a.users[c], &a.producers[c], &a.area[c]);
                    ac += &format!(
                        "{},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{:.4},{:.4},{:.4},{:.4}\n",
                        csv_field(&a.classes[c]),
                        u.est,
                        u.se,
                        u.hi - u.est,
                        pr.est,
                        pr.se,
                        pr.hi - pr.est,
                        ar.mapped,
                        ar.est,
                        ar.se,
                        ar.ci
                    );
                    h += &format!(
                        "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{} &plusmn; {}</td></tr>",
                        html_esc(&a.classes[c]),
                        fmt_pm(u),
                        fmt_pm(pr),
                        fmt_area(ar.mapped),
                        fmt_area(ar.est),
                        fmt_area(ar.ci)
                    );
                    labels.push(a.classes[c].clone());
                    series[0].push(ar.mapped);
                    series[1].push(if ar.est.is_finite() { ar.est } else { 0.0 });
                    errs[0].push(0.0);
                    errs[1].push(if ar.ci.is_finite() { ar.ci } else { 0.0 });
                }
                h += "</table>";
                h += &svg_bars(
                    &labels,
                    &series,
                    &errs,
                    &["mapped".into(), format!("adjusted (+- {pct}%)")],
                    &p.strata.unit,
                );
                write(&format!("{tag}_accuracy.csv"), &ac)?;
                if !m.by_year.is_empty() {
                    let mut yc = String::from("year,area,se,ci\n");
                    let (mut yl, mut ys, mut ye) = (Vec::new(), vec![Vec::new()], vec![Vec::new()]);
                    for (y, ar) in &m.by_year {
                        yc += &format!("{y},{:.4},{:.4},{:.4}\n", ar.est, ar.se, ar.ci);
                        yl.push(y.to_string());
                        ys[0].push(ar.est);
                        ye[0].push(ar.ci);
                    }
                    write(&format!("{tag}_change_by_year.csv"), &yc)?;
                    h += "<p>Area of change by year (the reference's dates):</p>";
                    h += &svg_bars(&yl, &ys, &ye, &[format!("area of change (+- {pct}%)")], &p.strata.unit);
                }
            }
            j.set_progress(0.7);
            // interpreters
            if who.len() >= 2 {
                let mut kc = String::from("a,b,points,agreement,kappa\n");
                h += "<h2>Interpreters</h2><table><tr><th>interpreters</th><th>points</th><th>agreement</th><th>kappa</th></tr>";
                for i in 0..who.len() {
                    for jj in i + 1..who.len() {
                        let (la, lb) = (&p.labels[&who[i]], &p.labels[&who[jj]]);
                        let lab = |l: &Vec<val::Label>, q: usize| l.get(q).map_or(String::new(), |x| x.label.clone());
                        let a: Vec<String> = (0..p.points.len()).map(|q| lab(la, q)).collect();
                        let b: Vec<String> = (0..p.points.len()).map(|q| lab(lb, q)).collect();
                        let g = val::cohen(&a, &b);
                        kc += &format!(
                            "{},{},{},{:.4},{:.4}\n",
                            csv_field(&who[i]),
                            csv_field(&who[jj]),
                            g.n,
                            g.observed,
                            g.kappa
                        );
                        h += &format!(
                            "<tr><td>{}</td><td>{}</td><td>{:.1}%</td><td>{:.3}</td></tr>",
                            html_esc(&format!("{} / {}", who[i], who[jj])),
                            g.n,
                            100.0 * g.observed,
                            g.kappa
                        );
                    }
                }
                h += "</table>";
                write("interpreters.csv", &kc)?;
            }
            h += &format!("<h2>References</h2><p class=\"small\">{}</p></body></html>", html_esc(REFERENCES));
            let h = h.replace('\n', "<br>");
            write("report.html", &h)?;
            j.set_progress(1.0);
            Ok(())
        });
        self.exports.push(job.clone());
        self.show_exports = true;
        Some(job)
    }
}
