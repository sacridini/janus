//! The validation guide's screenshots (`--shots validation DATA OUT`; src/app_validation.cpp,
//! shotsValidationStep): site/validation.html walks through the validation of Global Forest Change's loss of
//! 2018-2023 near Novo Progresso with Sentinel-2 dry-season composites (tools/make_tutorial_data.py), one stage per
//! step. The points are labelled by a rule on each one's NBR series (a drop from forest, and whether it grows
//! back), not by a person: the numbers on the screenshots show the tool at work, they validate nothing.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use egui::Context;
use gdal::spatial_ref::{AxisMappingStrategy, CoordTransform, SpatialRef};
use janus_cube::BandSelection;
use janus_validation as val;

use crate::app::{State, Tab};
use crate::composite::BandComposite;
use crate::exports::{ExportJob, ExportState};
use crate::shots::Shots;
use crate::validation::EVENT_LABELS;

/// What the tour keeps between its stages.
#[derive(Default)]
pub struct ValShots {
    /// its work folder (the project, the report)
    dir: PathBuf,
    /// what each point shows (an index of EVENT_LABELS), its year of change * 10 + the confidence
    labels: Vec<usize>,
    years: Vec<i32>,
    /// the point interpreted on the screenshot, and its date
    example: Option<usize>,
    example_t: usize,
    report: Option<Arc<ExportJob>>,
    /// the stage's screenshot taken (what the stage changes next comes after it)
    shot: bool,
}

/// The NBR, (NIR - SWIR2) / (NIR + SWIR2) (bands 4 and 6), of each composite at each point (x, y in the CRS
/// `wkt`); NaN without a clear observation.
fn nbr_at(files: &[String], wkt: &str, pts: &[val::Point]) -> Vec<Vec<f64>> {
    let mut out = vec![vec![f64::NAN; files.len()]; pts.len()];
    let Ok(mut from) = SpatialRef::from_wkt(wkt) else { return out };
    from.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    for (k, f) in files.iter().enumerate() {
        let Ok(ds) = janus_cube::open_raster(f) else { continue };
        let Ok(mut to) = ds.spatial_ref() else { continue };
        to.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
        let (Ok(ct), Ok(gt)) = (CoordTransform::new(&from, &to), ds.geo_transform()) else { continue };
        let (Ok(b4), Ok(b6)) = (ds.rasterband(4), ds.rasterband(6)) else { continue };
        let (w, h) = ds.raster_size();
        for (i, p) in pts.iter().enumerate() {
            let (mut x, mut y) = ([p.x], [p.y]);
            if ct.transform_coords(&mut x, &mut y, &mut []).is_err() {
                continue;
            }
            let col = ((x[0] - gt[0]) / gt[1]).floor();
            let row = ((y[0] - gt[3]) / gt[5]).floor();
            if col < 0.0 || row < 0.0 || col >= w as f64 || row >= h as f64 {
                continue;
            }
            let at = (col as isize, row as isize);
            let read = |b: &gdal::raster::RasterBand| {
                b.read_as::<i16>(at, (1, 1), (1, 1), None).ok().map_or(0, |v| v.data()[0])
            };
            let (nir, swir) = (read(&b4), read(&b6));
            if nir > 0 && swir > 0 {
                out[i][k] = f64::from(nir - swir) / f64::from(nir + swir);
            }
        }
    }
    out
}

/// The centre of a pixel of `year` in the map `path` well inside a clearing of that year (the most pixels of it in
/// a 9 x 9 window, then the nearest the centre), in the map's CRS, and that CRS.
fn inside_loss(path: &str, year: i32) -> Option<(f64, f64, String)> {
    let ds = janus_cube::open_raster(path).ok()?;
    let (w, h) = ds.raster_size();
    let a = ds.rasterband(1).ok()?.read_as::<i32>((0, 0), (w, h), (w, h), None).ok()?.into_shape_and_vec().1;
    let gt = ds.geo_transform().ok()?;
    let wkt = ds.spatial_ref().ok()?.to_wkt().ok()?;
    let (mut best, mut bx, mut by) = (0, -1i64, -1i64);
    let (wi, hi) = (w as i64, h as i64);
    for j in (4..hi - 4).step_by(2) {
        for i in (4..wi - 4).step_by(2) {
            if a[(j * wi + i) as usize] != year {
                continue;
            }
            let mut n = 0;
            for dy in -4..=4 {
                for dx in -4..=4 {
                    n += i32::from(a[((j + dy) * wi + i + dx) as usize] == year);
                }
            }
            let far = (i - wi / 2).abs() + (j - hi / 2).abs();
            if n > best || (n == best && far < (bx - wi / 2).abs() + (by - hi / 2).abs()) {
                (best, bx, by) = (n, i, j);
            }
        }
    }
    (bx >= 0).then(|| (gt[0] + (bx as f64 + 0.5) * gt[1], gt[3] + (by as f64 + 0.5) * gt[5], wkt))
}

/// What an interpreter would call a series of yearly NBR starting in `first`: a class of EVENT_LABELS (0 stable
/// forest, 1 deforestation, 2 degradation, 3 regeneration, 5 pasture, 7 no data, 8 other), the year it changed
/// (0: none) and the confidence (1-3).
fn read_series(nbr: &[f64], first: i32) -> (usize, i32, i32) {
    const FOREST: f64 = 0.45;
    if nbr.iter().any(|x| !x.is_finite()) {
        return (7, 0, 1);
    }
    for k in 1..nbr.len() {
        let drop = nbr[k - 1] - nbr[k];
        if nbr[k - 1] > FOREST && nbr[k] < FOREST - 0.05 && drop > 0.15 {
            // forest again two years on: the canopy opened (logging, fire) and closed
            let back = k + 2 < nbr.len() && nbr[k + 2] > FOREST && nbr[nbr.len() - 1] > FOREST;
            return (
                if back { 2 } else { 1 },
                first + k as i32,
                if drop > 0.3 {
                    3
                } else if drop > 0.2 {
                    2
                } else {
                    1
                },
            );
        }
    }
    // a deep dip that lasts a year and stays above cleared land: the canopy thinned (degradation), at its first
    // year
    for k in 1..nbr.len().saturating_sub(1) {
        let before = nbr[..k].iter().copied().fold(f64::MIN, f64::max);
        if before > FOREST + 0.1 && before - nbr[k] > 0.15 && nbr[k] < 0.5 && before - nbr[k + 1] > 0.08 {
            return (2, first + k as i32, 1);
        }
    }
    let lo = nbr.iter().copied().fold(f64::MAX, f64::min);
    let hi = nbr.iter().copied().fold(f64::MIN, f64::max);
    if lo > FOREST {
        (0, 0, if lo > FOREST + 0.1 { 3 } else { 2 })
    } else if nbr[0] < FOREST && nbr[nbr.len() - 1] > FOREST {
        (3, 0, 2)
    } else if hi < FOREST {
        (5, 0, if hi < FOREST - 0.1 { 3 } else { 2 })
    } else {
        (8, 0, 1)
    }
}

impl State {
    /// Nothing loading and nothing of the validation under way.
    fn shots_val_quiet(&self) -> bool {
        let v = &self.val;
        self.shots_quiet() && !v.strata_job.busy() && !v.draw_job.busy() && v.to_open.is_empty() && v.idle()
    }

    /// The date of `year` (else the last).
    fn shots_year_index(&self, year: i32) -> usize {
        let Some(s) = &self.session else { return 0 };
        s.info.layers.iter().position(|l| val::year_of(&l.label) == year).unwrap_or(s.info.t() - 1)
    }

    /// A point labelled as the rule read it (the current one is set to it).
    fn shots_label(&mut self, sh: &Shots, i: usize) {
        self.labels.current = Some(i);
        self.interp_label(Some(sh.val.labels[i]));
        let year = sh.val.years[i] / 10;
        let date = if year != 0 {
            let t = self.shots_year_index(year);
            self.session.as_ref().map_or(String::new(), |s| s.info.layers[t].label.clone())
        } else {
            String::new()
        };
        self.labels.entries[i].date = date;
        self.interp_set_confidence(sh.val.years[i] % 10);
    }

    fn shots_layer_of(&self, path: &str) -> Option<usize> {
        self.layers.iter().position(|l| l.inputs.len() == 1 && l.inputs[0] == path)
    }

    pub(crate) fn shots_validation(&mut self, sh: &mut Shots, ctx: &Context) -> Option<i32> {
        let fail = |sh: &Shots, what: &str| {
            println!("FAIL (stage {}): {what}", sh.stage);
            Some(1)
        };
        let t = sh.time();
        if t > 120.0 {
            return fail(sh, "timeout");
        }
        let data = PathBuf::from(&sh.input);
        let gfc = data.join("gfc_loss_2018_2023.tif").to_string_lossy().into_owned();
        let mapbiomas = data.join("mapbiomas_loss_2018_2023.tif").to_string_lossy().into_owned();
        let n_dates = |s: &State| s.session.as_ref().map_or(0, |s| s.info.t());
        let label_of =
            |s: &State, t: usize| s.session.as_ref().map_or(String::new(), |s| s.info.layers[t].label.clone());
        let panel = || Shots::fit(Tab::Validation);
        match sh.stage {
            0 => {
                // the composites, the NBR on the chart (NIR and SWIR2: bands 4 and 6)
                self.settings.s.interpreter = "Ana".into();
                self.labels.advance = false;
                sh.val.dir = sh.out.join("work");
                let _ = std::fs::remove_dir_all(&sh.val.dir);
                let _ = std::fs::create_dir_all(&sh.val.dir);
                let sel = BandSelection { band: 4, nd_band: 6, chosen: true, ..Default::default() };
                self.shots_open(&data.join("s2").to_string_lossy(), sel, ctx);
                sh.next("the Sentinel-2 composites opening (NBR)");
            }
            1 => {
                // in SWIR1 / NIR / red; the map to validate over them
                if self.opening.is_some() || self.layers.len() != 1 || self.session.is_none() {
                    return None;
                }
                let id = self.session.as_ref()?.info.id;
                self.composite = Some(BandComposite::of(id, [5, 4, 3]));
                self.shots_add(&gfc, ctx);
                sh.next(&format!(
                    "{} dates in SWIR1 / NIR / red; {} added as a layer",
                    n_dates(self),
                    PathBuf::from(&gfc).file_name()?.to_string_lossy()
                ));
            }
            2 => {
                // the tutorial's panels: Validation and Interpretation on the right, Chips under the map
                if self.opening.is_some() || self.layers.len() != 2 {
                    return None;
                }
                sh.layout = Some((vec![Tab::Validation, Tab::Interpretation], vec![Tab::Chips]));
                self.set_active(0, ctx);
                self.set_date(n_dates(self) - 1);
                sh.next("the tutorial's layout; the last date");
            }
            3 => {
                if sh.frames == 3 {
                    self.map.fit();
                    // a pin inside a clearing of 2021: its series on the chart
                    if let Some((x, y, wkt)) = inside_loss(&gfc, 2021) {
                        let info = self.session.as_ref()?.info.clone();
                        let to = |w: &str| {
                            SpatialRef::from_wkt(w).map(|mut s| {
                                s.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
                                s
                            })
                        };
                        let (mut xs, mut ys) = ([x], [y]);
                        if let (Ok(a), Ok(b)) = (to(&wkt), to(&info.crs_wkt))
                            && let Ok(ct) = CoordTransform::new(&a, &b)
                            && ct.transform_coords(&mut xs, &mut ys, &mut []).is_ok()
                            && let Some((px, py)) = info.geo_to_pixel(xs[0], ys[0])
                            && px >= 0.0
                            && py >= 0.0
                        {
                            self.add_pin(px as usize, py as usize);
                        }
                    }
                }
                if !self.shots_val_quiet() || t < 4.0 {
                    return None;
                }
                let size = ctx.content_rect().size() * ctx.pixels_per_point();
                if size.x.round() != crate::shots::SIZE[0] || size.y.round() != crate::shots::SIZE[1] {
                    println!(
                        "    note: the frame is {} x {}, not {} x {} (display scaling?)",
                        size.x,
                        size.y,
                        crate::shots::SIZE[0],
                        crate::shots::SIZE[1]
                    );
                }
                sh.shoot("01-series-and-map", &[]);
                sh.next("the series and the map");
            }
            4 => {
                // the Validation menu, open, the pointer on its first item
                if !self.shots_menu_shot(sh, "Validation", "02-menu", true) {
                    if t > 10.0 {
                        return fail(sh, "the Validation menu did not open");
                    }
                    return None;
                }
                sh.next("the Validation menu");
            }
            5 => {
                // Design: the map read as events in two periods, a buffer of 1 px
                if sh.frames == 1 {
                    egui::Popup::close_all(ctx);
                    self.val.show = true;
                    self.val.select_tab = Some(0);
                    let mut r = crate::validation::file_rule(&gfc);
                    r.kind = val::MapKind::Events;
                    r.bins = vec![2018.0, 2021.0, 2024.0];
                    let v = &mut self.val;
                    v.bins_text = vec![val::bins_text(&r.bins)];
                    v.maps = vec![r];
                    v.change_text = vec![String::new()];
                    v.buffer = 1;
                    v.limit = 0;
                    return None;
                }
                if sh.frames == 3 {
                    sh.focus.push(Tab::Validation);
                }
                if t < 2.0 {
                    return None;
                }
                sh.shoot("03-design", &[("panel", panel())]);
                sh.next("the design, before the strata");
            }
            6 => {
                // the strata, the sample size and its allocation
                if sh.frames == 1 {
                    let spec = match self.validation_spec() {
                        Ok(s) => s,
                        Err(e) => return fail(sh, &e),
                    };
                    self.validation_compute_strata(spec, ctx);
                    let v = &mut self.val;
                    v.strata_for = format!(
                        "{}|{}|{}|",
                        serde_json::Value::Array(v.maps.iter().map(val::MapRule::to_json).collect()),
                        v.buffer,
                        v.limit
                    );
                    return None;
                }
                if self.val.strata_job.busy() || !self.val.to_open.is_empty() || self.opening.is_some() {
                    return None;
                }
                if !self.val.design_error.is_empty() {
                    return fail(sh, &self.val.design_error.clone());
                }
                let v = &mut self.val;
                v.alloc = val::Alloc::Proportional as usize;
                v.target_se = 0.015;
                v.n_auto = true;
                v.min_per = 50;
                v.seed = 42;
                for s in &mut v.strata.strata {
                    s.expected_ua = if s.change {
                        0.8
                    } else if s.name.contains("buffer") {
                        0.85
                    } else {
                        0.95
                    };
                }
                if let Some(i) = self.shots_layer_of(&self.val.strata_path.clone()) {
                    self.layers[i].opacity = 0.8;
                }
                if !self.shots_val_quiet() || t < 4.0 {
                    return None;
                }
                let strata: String = self
                    .val
                    .strata
                    .strata
                    .iter()
                    .map(|s| format!("\n      {}: {} px, {} points", s.name, s.pixels, s.n))
                    .collect();
                sh.shoot("04-strata", &[("panel", panel())]);
                sh.next(&format!("the strata:{strata}"));
            }
            7 => {
                // the sample, drawn into a project; the interpretation starts, blind
                if sh.frames == 1 {
                    let spec = match self.validation_spec() {
                        Ok(s) => s,
                        Err(e) => return fail(sh, &e),
                    };
                    let path = sh.val.dir.join("gfc_loss_2018_2023_validation.gpkg");
                    self.validation_draw(&path.to_string_lossy(), spec, ctx);
                    return None;
                }
                if self.val.draw_job.busy() || !self.val.to_open.is_empty() || self.opening.is_some() {
                    return None;
                }
                if !self.val.design_error.is_empty() {
                    return fail(sh, &self.val.design_error.clone());
                }
                if self.val.project.is_none() || !self.labels.project {
                    return fail(sh, "no project, or its interpretation did not start");
                }
                if sh.val.labels.is_empty() {
                    // what each point shows, read once (labelled in stage 9)
                    let info = self.session.as_ref()?.info.clone();
                    let p = self.val.project.as_ref()?;
                    let files: Vec<String> = info.layers.iter().map(|l| l.path.clone()).collect();
                    let nbr = nbr_at(&files, &p.strata.grid.wkt, &p.points);
                    let first = val::year_of(&info.layers[0].label);
                    let mut count: BTreeMap<usize, usize> = BTreeMap::new();
                    for series in &nbr {
                        let (label, year, confidence) = read_series(series, first);
                        sh.val.labels.push(label);
                        sh.val.years.push(year * 10 + confidence);
                        *count.entry(label).or_insert(0) += 1;
                    }
                    let c: String = count.iter().map(|(l, n)| format!(" {} {n};", EVENT_LABELS[*l])).collect();
                    println!("    the points read:{c}");
                    self.val.show = true;
                    self.val.select_tab = Some(0);
                    sh.focus.push(Tab::Validation);
                    self.map.fit();
                }
                if !self.shots_val_quiet() || t < 4.0 {
                    return None;
                }
                sh.shoot("05-sample", &[("panel", panel())]);
                let n = self.val.project.as_ref()?.points.len();
                sh.next(&format!("the sample: {n} points, blind"));
            }
            8 => {
                // one point being interpreted: a clearing, at the date it shows
                if sh.frames == 1 {
                    // a clearing in a stratum of change, seen with high confidence, in the middle of the series
                    let p = self.val.project.as_ref()?;
                    sh.val.example = (0..p.points.len()).find(|&i| {
                        let (year, conf) = (sh.val.years[i] / 10, sh.val.years[i] % 10);
                        let change =
                            p.strata.index_of_code(p.points[i].stratum).is_some_and(|s| p.strata.strata[s].change);
                        sh.val.labels[i] == 1 && conf == 3 && (2020..=2022).contains(&year) && change
                    });
                    let Some(ex) = sh.val.example else {
                        return fail(sh, "no clearing in a stratum of change to show");
                    };
                    self.clear_pins(); // the first screenshot's
                    self.labels.show = true;
                    self.val.show_chips = true;
                    self.labels.zoom = 6.0;
                    self.interp_go(ex);
                    sh.val.example_t = self.shots_year_index(sh.val.years[ex] / 10);
                    self.set_date(sh.val.example_t);
                    return None;
                }
                let ex = sh.val.example?;
                if sh.frames == 3 {
                    sh.focus.push(Tab::Interpretation);
                }
                if sh.frames == 4 {
                    sh.focus.push(Tab::Chips);
                }
                if t < 3.0 {
                    return None;
                }
                if self.labels.entries[ex].label.is_empty() {
                    self.interp_label(Some(1));
                    self.interp_set_date(false);
                    self.interp_set_confidence(3);
                    return None;
                }
                if !self.shots_val_quiet() || !self.val.chips.has_texture() || t < 5.0 {
                    return None;
                }
                sh.shoot(
                    "06-interpretation",
                    &[
                        ("panel", Shots::tab(Tab::Interpretation)),
                        ("chips", Shots::tab(Tab::Chips)),
                        ("chart", Shots::tab(Tab::TimeSeries)),
                    ],
                );
                sh.next(&format!(
                    "point {} interpreted: deforestation in {}",
                    ex + 1,
                    label_of(self, sh.val.example_t)
                ));
            }
            9 => {
                // every point labelled (by the rule), then the accuracy
                if sh.frames == 1 {
                    // in a random order, a label every 40 s or so (the order the convergence chart follows)
                    let n = self.val.project.as_ref()?.points.len();
                    let mut order: Vec<usize> = (0..n).collect();
                    let mut rng = janus_zeit::rng::Mt19937::new(7);
                    for i in (1..n).rev() {
                        order.swap(i, rng.next_u32() as usize % (i + 1));
                    }
                    let mut k = 0;
                    for i in order {
                        if Some(i) == sh.val.example {
                            continue;
                        }
                        self.shots_label(sh, i);
                        k += 1;
                        let s = 9 * 3600 + 40 * k;
                        self.labels.entries[i].updated = format!(
                            "2026-10-{:02}T{:02}:{:02}:{:02}Z",
                            1 + s / 86400,
                            s / 3600 % 24,
                            s / 60 % 60,
                            s % 60
                        );
                        self.validation_labels_changed(i);
                    }
                    self.save_labels();
                    self.labels.current = sh.val.example;
                    self.val.show_chips = false;
                    self.val.select_tab = Some(1);
                    self.val.sections = Some(0);
                    return None;
                }
                if sh.frames == 3 {
                    sh.focus.push(Tab::Validation);
                }
                if !self.shots_val_quiet() || t < 3.0 {
                    return None;
                }
                sh.shoot("07-accuracy", &[("panel", panel())]);
                sh.next("every point labelled; the accuracy");
            }
            10 => {
                // how the labels read for this map
                self.val.sections = Some(1);
                if t < 2.0 {
                    return None;
                }
                sh.shoot("-", &[("08-translations", panel())]);
                sh.next("the translations of the labels");
            }
            11 | 12 => {
                // the estimates as the labels came, and the dates; the area of change by year
                self.val.sections = Some(if sh.stage == 11 { 2 | 4 } else { 8 });
                if sh.frames > 3 {
                    sh.scroll = Some((Tab::Validation, f32::MAX));
                }
                if t < 2.0 {
                    return None;
                }
                if sh.stage == 11 {
                    sh.shoot("-", &[("09-convergence-dates", panel())]);
                    sh.next("the convergence and the dates");
                } else {
                    sh.shoot("-", &[("10-by-year", panel())]);
                    sh.next("the area of change by year");
                }
            }
            13 => {
                // not blind any more: the map and the points right or wrong
                if sh.frames == 1 {
                    self.val.sections = Some(0);
                    sh.scroll = Some((Tab::Validation, 0.0));
                    self.set_validation_blind(false);
                    self.val.color_by = 2;
                    let strata = self.val.project.as_ref()?.strata_path.clone();
                    if let Some(i) = self.shots_layer_of(&strata) {
                        self.layers[i].visible = false;
                    }
                    if let Some(p) = self.labels.pin.take() {
                        self.remove_pin(p);
                    }
                    return None;
                }
                if sh.frames == 3 {
                    self.map.fit();
                    self.set_date(n_dates(self) - 1);
                }
                if !self.shots_val_quiet() || t < 4.0 {
                    return None;
                }
                sh.shoot("11-right-wrong", &[]);
                sh.next("the points right or wrong over the map");
            }
            14 => {
                // a second map, assessed with the same sample
                if sh.frames == 1 {
                    let mut rule = crate::validation::file_rule(&mapbiomas);
                    rule.kind = val::MapKind::Events;
                    rule.bins = vec![2018.0, 2021.0, 2024.0];
                    let p = self.val.project.as_mut()?;
                    p.maps.push(val::AssessedMap { rule, mapping: BTreeMap::new() });
                    let k = p.maps.len() - 1;
                    self.validation_add_map_data(k, ctx);
                    self.val.meta_dirty = true;
                    self.val.version += 1;
                    self.val.select_tab = Some(2);
                    return None;
                }
                let v = &self.val;
                let ready = v.data.get(1).is_some_and(|d| d.ready())
                    && v.project.as_ref().is_some_and(|p| p.maps.get(1).is_some_and(|m| !m.mapping.is_empty()));
                if !ready || !self.shots_val_quiet() || t < 3.0 {
                    return None;
                }
                sh.shoot("-", &[("12-maps", panel())]);
                sh.next("MapBiomas assessed with the same sample");
            }
            15 => {
                // a second interpreter's labels, merged
                if sh.frames == 1 {
                    if !self.val.idle() {
                        sh.frames = 0; // the project written first
                        return None;
                    }
                    let p = self.val.project.as_ref()?;
                    let copy =
                        sh.val.dir.join("gfc_loss_2018_2023_validation_bruno.gpkg").to_string_lossy().into_owned();
                    if let Err(e) = std::fs::copy(&p.path, &copy) {
                        return fail(sh, &format!("the copy: {e}"));
                    }
                    // Bruno labels 60 points across the strata, and reads a few clearings as degradation, some
                    // pasture as agriculture
                    let n = p.points.len();
                    let mut rows = Vec::new();
                    let mut changed = 0;
                    for k in 0..60.min(n) {
                        let i = k * n / 60;
                        let mut l = self.labels.entries[i].clone();
                        if changed < 5 && k % 2 == 1 && (sh.val.labels[i] == 1 || sh.val.labels[i] == 5) {
                            l.label = if sh.val.labels[i] == 1 { "Degradation" } else { "Agriculture" }.into();
                            changed += 1;
                        }
                        l.updated = val::now_iso();
                        rows.push((i, l));
                    }
                    if let Err(e) = val::save_labels(&copy, "Bruno", &rows, &[]) {
                        return fail(sh, &e);
                    }
                    let (_, msg) = self.validation_merge(&copy);
                    self.val.message = msg;
                    self.val.select_tab = Some(3);
                    return None;
                }
                if !self.shots_val_quiet() || t < 3.0 {
                    return None;
                }
                sh.shoot("-", &[("13-interpreters", panel())]);
                sh.next(&format!("Bruno's labels merged: {}", self.val.message));
            }
            16 => {
                // adjudication
                if sh.frames == 1 {
                    self.val.message.clear();
                    self.set_validation_blind(true);
                    self.start_adjudication();
                    if self.labels.filter.is_empty() {
                        return fail(sh, "no disagreement to adjudicate");
                    }
                    sh.val.shot = false;
                    return None;
                }
                if sh.frames == 3 {
                    sh.focus.push(Tab::Interpretation);
                }
                if !sh.val.shot {
                    if !self.shots_val_quiet() || t < 4.0 {
                        return None;
                    }
                    sh.shoot("14-adjudication", &[("panel", Shots::tab(Tab::Interpretation))]);
                    sh.val.shot = true;
                    return None;
                }
                // each disagreement settled as Ana read it
                for i in self.labels.filter.clone() {
                    self.shots_label(sh, i);
                }
                self.save_labels();
                self.labels.who = self.interpreter_name();
                self.validation_entries();
                self.labels.filter.clear();
                self.labels.filter_name.clear();
                sh.next("the disagreements adjudicated");
            }
            17 => {
                // the report
                if sh.frames == 1 {
                    self.set_validation_blind(false);
                    let dir = sh.val.dir.join("report");
                    let _ = std::fs::create_dir_all(&dir);
                    sh.val.report = self.export_validation_report(&dir.to_string_lossy(), ctx);
                    if sh.val.report.is_none() {
                        return fail(sh, "the report did not start");
                    }
                    self.val.select_tab = Some(4);
                    return None;
                }
                if sh.frames == 3 {
                    sh.focus.push(Tab::Validation);
                }
                self.show_exports = false;
                let report = sh.val.report.clone()?;
                if report.state() == ExportState::Running || !self.shots_val_quiet() || t < 3.0 {
                    return None;
                }
                if report.state() != ExportState::Done {
                    return fail(sh, &format!("the report failed: {}", report.error()));
                }
                sh.shoot("-", &[("15-report", panel())]);
                sh.next(&format!("the report: {}", sh.val.dir.join("report").join("report.html").display()));
            }
            _ => {
                self.close_validation_project();
                println!("OK");
                return Some(0);
            }
        }
        None
    }
}
