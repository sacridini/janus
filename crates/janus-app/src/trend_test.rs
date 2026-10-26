//! Analysis > Trend test: whether the trend of a region, or of each class of a categorical layer, differs from 0,
//! and whether the classes' trends differ, by PARTS (remotePARTS, Ives et al. 2021; janus_zeit::trend_test). The
//! series of the region's pixels are read on a thread (at full resolution, or one pixel in k x k of a large
//! region), with the classes under them; each pixel's AR(1) trend, the spatial correlation and the partitioned
//! GLS follow there.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use egui::{Color32, RichText, Ui};
use janus_cube::multidim::format_g;
use janus_cube::{CubeInfo, with_thread_reader};
use janus_zeit::trend_test::{self, Gaps, Outcome, Pixels, Settings};

use crate::app::State;

/// Pixels read from a region at most (one in k x k beyond), and values (pixels x dates).
const MAX_PIXELS: f64 = 500_000.0;
const MAX_VALUES: f64 = 40_000_000.0;

pub struct TrendTestUi {
    /// 0 the whole image, 1 the visible area, 2 the ROI
    pub scope: usize,
    /// the classes' layer (its cube id); 0: none, the region's mean
    pub class_cube: u64,
    pub per_year: bool,
    pub harmonics: usize,
    pub exact_gaps: bool,
    pub partsize: usize,
    pub max_partitions: usize,
    pub ml_nugget: bool,
    /// run when the series is ready (--analysis trend,run), on the first categorical layer's classes (,classes)
    pub auto_run: bool,
    pub auto_classes: bool,
    pub job: Option<Arc<TrendJob>>,
    pub result: Option<Result<TrendResult, String>>,
}

impl Default for TrendTestUi {
    fn default() -> Self {
        TrendTestUi {
            scope: 2,
            class_cube: 0,
            per_year: true,
            harmonics: 0,
            exact_gaps: true,
            partsize: 1500,
            max_partitions: 100,
            ml_nugget: true,
            auto_run: false,
            auto_classes: false,
            job: None,
            result: None,
        }
    }
}

/// A test running on its thread.
pub struct TrendJob {
    /// the fraction done and the stage
    pub progress: Mutex<(f64, String)>,
    pub done: Mutex<Option<Result<TrendResult, String>>>,
    pub cancel: AtomicBool,
}

pub struct TrendResult {
    pub outcome: Outcome,
    /// each class's name and colour
    pub classes: BTreeMap<i32, (String, [u8; 3])>,
    pub class_layer: Option<String>,
    /// what was read: the region, the pixels, the sampling
    pub region: String,
    /// the trend's unit ("/yr", "/step")
    pub unit: &'static str,
    /// the units of the correlation's range
    pub distance_unit: &'static str,
    pub seconds: f64,
}

/// The classes' layer: its cube, its cache, its date, the active pixel -> its pixel (x = ax + bx x'), the
/// classes' names and colours, the layer's name.
type ClassSource =
    (Arc<CubeInfo>, Arc<janus_cache::FullResCache>, usize, [f64; 4], BTreeMap<i32, (String, [u8; 3])>, String);

/// What the thread reads.
struct Request {
    info: Arc<CubeInfo>,
    full: Arc<janus_cache::FullResCache>,
    window: [usize; 4],
    region: String,
    poly: janus_geo::plane::Polygon,
    classes: Option<ClassSource>,
    settings: Settings,
}

/// A window of one date of a cube, `(bw, bh)` pixels (nearest neighbour), from the full-resolution cache or the
/// file, NaN where missing.
fn read(
    info: &Arc<CubeInfo>,
    full: &janus_cache::FullResCache,
    t: usize,
    win: (usize, usize, usize, usize),
    bw: usize,
    bh: usize,
) -> Vec<f32> {
    let mut buf = vec![f32::NAN; bw * bh];
    let ok = full.read_window(t, win, &mut buf, (bw, bh))
        || with_thread_reader(info, |r| r.read_window(t, win, &mut buf, (bw, bh)));
    if !ok {
        buf.fill(f32::NAN);
    }
    buf
}

/// The pixels' places in km: a projected CRS's units scaled to metres, or longitude and latitude projected
/// (equirectangular, at the region's mean latitude); None without a georeference (pixels then).
fn places(info: &CubeInfo, px: &[(f64, f64)]) -> (Vec<f64>, Vec<f64>, &'static str) {
    let srs = (!info.crs_wkt.is_empty()).then(|| gdal::spatial_ref::SpatialRef::from_wkt(&info.crs_wkt).ok()).flatten();
    let geo: Option<Vec<(f64, f64)>> = px.iter().map(|&(x, y)| info.pixel_to_geo(x, y)).collect();
    match (geo, srs) {
        (Some(g), Some(srs)) if srs.is_geographic() => {
            let lat0 = g.iter().map(|p| p.1).sum::<f64>() / g.len().max(1) as f64;
            let k = 6371.0088 * std::f64::consts::PI / 180.0;
            (g.iter().map(|p| k * lat0.to_radians().cos() * p.0).collect(), g.iter().map(|p| k * p.1).collect(), "km")
        }
        (Some(g), Some(srs)) => {
            let m = srs.linear_units() / 1000.0;
            (g.iter().map(|p| m * p.0).collect(), g.iter().map(|p| m * p.1).collect(), "km")
        }
        _ => (px.iter().map(|p| p.0).collect(), px.iter().map(|p| p.1).collect(), "pixels"),
    }
}

/// The thread: the region's series and classes read, then the test.
fn work(req: Request, job: &TrendJob, ctx: &egui::Context) -> Result<TrendResult, String> {
    let started = Instant::now();
    let progress = |f: f64, m: &str| {
        *job.progress.lock().unwrap() = (f, m.to_string());
        ctx.request_repaint();
    };
    let info = &req.info;
    let [x0, y0, x1, y1] = req.window;
    let (w, h) = (x1 - x0, y1 - y0);
    let nt = info.t();
    let budget = MAX_PIXELS.min(MAX_VALUES / nt as f64);
    let s = (budget / (w as f64 * h as f64)).sqrt().min(1.0);
    let (bw, bh) = (((w as f64 * s) as usize).max(1), ((h as f64 * s) as usize).max(1));
    let win = (x0, y0, w, h);
    let mask = if req.poly.is_empty() {
        vec![true; bw * bh]
    } else {
        janus_geo::plane::mask(&req.poly, x0 as f64, y0 as f64, w as f64, h as f64, bw, bh)
    };
    let kept: Vec<usize> = (0..bw * bh).filter(|&k| mask[k]).collect();
    if kept.is_empty() {
        return Err("no pixel in the region".into());
    }
    // the active pixel at each cell's centre
    let centre = |k: usize| {
        (
            x0 as f64 + ((k % bw) as f64 + 0.5) * w as f64 / bw as f64,
            y0 as f64 + ((k / bw) as f64 + 0.5) * h as f64 / bh as f64,
        )
    };
    let mut values = vec![f64::NAN; kept.len() * nt];
    for t in 0..nt {
        if job.cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        progress(0.4 * t as f64 / nt as f64, &format!("Reading the region: date {} of {nt}", t + 1));
        let buf = read(info, &req.full, t, win, bw, bh);
        for (j, &k) in kept.iter().enumerate() {
            values[j * nt + t] = f64::from(buf[k]);
        }
    }
    let px: Vec<(f64, f64)> = kept.iter().map(|&k| centre(k)).collect();
    let (x, y, distance_unit) = places(info, &px);
    // the classes under the pixels, from their layer's date over the region's bounds
    let mut class_layer = None;
    let mut names = BTreeMap::new();
    let classes = match &req.classes {
        None => None,
        Some((cinfo, cfull, ct, [ax, bx, ay, by], cnames, cname)) => {
            progress(0.4, "Reading the classes");
            let to_layer = |(x, y): (f64, f64)| ((x - ax) / bx, (y - ay) / by);
            let (a, b) = (to_layer((x0 as f64, y0 as f64)), to_layer((x1 as f64, y1 as f64)));
            let lx0 = (a.0.min(b.0).floor().max(0.0) as usize).min(cinfo.width - 1);
            let ly0 = (a.1.min(b.1).floor().max(0.0) as usize).min(cinfo.height - 1);
            let lx1 = (a.0.max(b.0).ceil() as usize).clamp(lx0 + 1, cinfo.width);
            let ly1 = (a.1.max(b.1).ceil() as usize).clamp(ly0 + 1, cinfo.height);
            let (cw, ch) = (lx1 - lx0, ly1 - ly0);
            let cs = (4.0e6 / (cw as f64 * ch as f64)).sqrt().min(1.0);
            let (cbw, cbh) = (((cw as f64 * cs) as usize).max(1), ((ch as f64 * cs) as usize).max(1));
            let buf = read(cinfo, cfull, *ct, (lx0, ly0, cw, ch), cbw, cbh);
            let class_at = |p: (f64, f64)| -> Option<i32> {
                let (lx, ly) = to_layer(p);
                let (i, j) = ((lx - lx0 as f64) * cbw as f64 / cw as f64, (ly - ly0 as f64) * cbh as f64 / ch as f64);
                if i < 0.0 || j < 0.0 || i >= cbw as f64 || j >= cbh as f64 {
                    return None;
                }
                let v = buf[j as usize * cbw + i as usize];
                (v.is_finite() && cnames.contains_key(&(v.round() as i32))).then(|| v.round() as i32)
            };
            class_layer = Some(cname.clone());
            names = cnames.clone();
            Some(px.iter().map(|&p| class_at(p)).collect())
        }
    };
    let pixels = Pixels { values, years: (0..nt).map(|t| info.decimal_year(t)).collect(), x, y, classes };
    let outcome = trend_test::run(&pixels, &req.settings, &|f, m| progress(0.45 + 0.55 * f, m), &job.cancel)?;
    let sampled = if s < 1.0 { format!(", one in {:.1} x {0:.1}", 1.0 / s) } else { String::new() };
    Ok(TrendResult {
        outcome,
        classes: names,
        class_layer,
        region: format!("{}: {} pixels read{sampled}", req.region, kept.len()),
        unit: if req.settings.per_year { "/yr" } else { "/step" },
        distance_unit,
        seconds: started.elapsed().as_secs_f64(),
    })
}

fn g(v: f64) -> String {
    if v.is_finite() { format_g(v, 4) } else { "-".into() }
}

impl State {
    /// The categorical layers on the active layer's grid (by an affine map): (layer, name).
    fn class_layers(&self) -> Vec<(usize, String)> {
        (0..self.layers.len())
            .filter(|&i| i == self.active || (self.layers[i].place.aligned && self.layers[i].place.reproj.is_none()))
            .filter(|&i| {
                let c = if i == self.active { &self.classes } else { &self.layers[i].classes };
                c.on() && !c.list.is_empty() && self.layer_session(i).is_some()
            })
            .map(|i| (i, self.layers[i].name.clone()))
            .collect()
    }

    /// Starts the test on the chosen region and classes.
    pub(crate) fn start_trend_test(&mut self, ctx: &egui::Context) {
        let Some(s) = &self.session else { return };
        let o = &self.analysis.trend;
        let scope = if o.scope == 2 && self.series.roi_rect.is_none() { 0 } else { o.scope };
        let (window, what) = self.run_window(scope);
        let poly = if scope == 2 { self.series.roi_poly.clone() } else { Default::default() };
        let region = if scope == 2 && !poly.is_empty() { "ROI".to_string() } else { what.to_string() };
        let classes = self.class_layers().into_iter().find_map(|(i, name)| {
            let cs = self.layer_session(i)?;
            if cs.info.id != o.class_cube {
                return None;
            }
            let (list, t, place) = if i == self.active {
                (&self.classes.list, self.display.t, [0.0, 1.0, 0.0, 1.0])
            } else {
                let l = &self.layers[i];
                (&l.classes.list, l.disp.t, [l.place.ax, l.place.bx, l.place.ay, l.place.by])
            };
            let names = list.iter().map(|e| (e.value, (e.name.clone(), e.color))).collect();
            Some((cs.info.clone(), cs.s.full_res.clone(), t.min(cs.info.t() - 1), place, names, name))
        });
        let threads = self.settings.s.threads;
        let settings = Settings {
            per_year: o.per_year && s.info.time_is_date,
            harmonics: o.harmonics,
            gaps: if o.exact_gaps { Gaps::Exact } else { Gaps::Ignore },
            partsize: o.partsize.max(50),
            max_partitions: o.max_partitions.max(2),
            nugget: (!o.ml_nugget).then_some(0.0),
            n_jobs: if threads > 0 { threads as i32 } else { -1 },
            ..Default::default()
        };
        let req = Request { info: s.info.clone(), full: s.s.full_res.clone(), window, region, poly, classes, settings };
        let job = Arc::new(TrendJob {
            progress: Mutex::new((0.0, "Starting".into())),
            done: Mutex::new(None),
            cancel: AtomicBool::new(false),
        });
        let (j, ctx) = (job.clone(), ctx.clone());
        let spawned = std::thread::Builder::new().name("trend-test".into()).spawn(move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(req, &j, &ctx)))
                .unwrap_or_else(|_| Err("the test failed (a panic: see the log)".into()));
            *j.done.lock().unwrap() = Some(r);
            ctx.request_repaint();
        });
        match spawned {
            Ok(_) => {
                self.analysis.trend.job = Some(job);
                self.analysis.trend.result = None;
            }
            Err(e) => self.analysis.trend.result = Some(Err(e.to_string())),
        }
    }

    /// The test's result, once its thread is done.
    pub(crate) fn pump_trend_test(&mut self) {
        let t = &mut self.analysis.trend;
        let done = t.job.as_ref().and_then(|job| job.done.lock().unwrap().take());
        if let Some(r) = done {
            t.result = Some(r);
            t.job = None;
        }
    }

    /// Whether the test asked from the command line is done (--screenshot).
    pub(crate) fn trend_test_settled(&self) -> bool {
        let t = &self.analysis.trend;
        !t.auto_run || (t.job.is_none() && t.result.is_some())
    }

    pub(crate) fn trend_test_ui(&mut self, ui: &mut Ui) {
        self.pump_trend_test();
        let Some(s) = &self.session else { return };
        // the series tested: the active layer's, a series of values
        let series = |s: &crate::session::Session, categorical: bool| s.info.t() >= 4 && !categorical && !s.info.stack;
        if !series(s, self.classes.on()) {
            let name = self.layers.get(self.active).map_or(String::new(), |l| l.name.clone());
            let other = (0..self.layers.len()).find(|&i| {
                i != self.active && self.layer_session(i).is_some_and(|s| series(s, self.layers[i].classes.on()))
            });
            // run from the command line: the first series made active (--layer makes the last layer opened active)
            if self.analysis.trend.auto_run
                && let Some(i) = other
            {
                self.set_active(i, ui.ctx());
                return;
            }
            ui.weak(format!(
                "The trend test runs on the active layer's series, and {name} is {}: make a series of values with \
                 4 dates or more active (click it in Layers).",
                if s.info.stack {
                    "a stack of variables, not a series".to_string()
                } else if s.info.t() < 4 {
                    format!("{} date(s)", s.info.t())
                } else {
                    "categorical".into()
                }
            ));
            return;
        }
        let time_is_date = s.info.time_is_date;
        let has_roi = self.series.roi_rect.is_some();
        let class_layers = self.class_layers();
        let ids: Vec<u64> =
            class_layers.iter().filter_map(|(i, _)| self.layer_session(*i).map(|s| s.info.id)).collect();
        let running = self.analysis.trend.job.is_some();
        let o = &mut self.analysis.trend;
        if !ids.contains(&o.class_cube) {
            o.class_cube = 0;
        }
        ui.horizontal_wrapped(|ui| {
            ui.label("Region");
            let scopes = ["Whole image", "Visible area", "ROI"];
            let scope = if o.scope == 2 && !has_roi { 0 } else { o.scope };
            egui::ComboBox::from_id_salt("trend scope").selected_text(scopes[scope]).show_ui(ui, |ui| {
                for (k, name) in scopes.iter().enumerate() {
                    if k < 2 || has_roi {
                        ui.selectable_value(&mut o.scope, k, *name);
                    }
                }
            });
            ui.label("Classes");
            let current = class_layers.iter().zip(&ids).find(|(_, id)| **id == o.class_cube).map(|((_, n), _)| n.clone());
            egui::ComboBox::from_id_salt("trend classes")
                .selected_text(current.unwrap_or_else(|| "None (the region's mean)".into()))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut o.class_cube, 0, "None (the region's mean)");
                    for ((_, name), id) in class_layers.iter().zip(&ids) {
                        ui.selectable_value(&mut o.class_cube, *id, name);
                    }
                })
                .response
                .on_hover_text("A categorical layer (land cover...) at its current date: the trend of each class, and whether they differ");
        });
        ui.horizontal_wrapped(|ui| {
            if time_is_date {
                ui.label("Trend per");
                ui.radio_value(&mut o.per_year, true, "year");
                ui.radio_value(&mut o.per_year, false, "time step");
            }
            ui.label("Harmonics");
            ui.add(egui::DragValue::new(&mut o.harmonics).range(0..=3))
                .on_hover_text("Pairs of harmonics of the yearly cycle, fitted with the trend (sub-annual series)");
            ui.checkbox(&mut o.exact_gaps, "Missing dates kept apart")
                .on_hover_text("Observations k dates apart correlated by rho^k (else the valid ones taken as consecutive, as remotePARTS)");
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Partitions of");
            ui.add(egui::DragValue::new(&mut o.partsize).range(100..=5000).suffix(" pixels"));
            ui.label("at most");
            ui.add(egui::DragValue::new(&mut o.max_partitions).range(2..=2000))
                .on_hover_text("More partitions add little once the correlation between them dominates");
            ui.checkbox(&mut o.ml_nugget, "Nugget by maximum likelihood")
                .on_hover_text("Each partition's nugget (the share of variance without spatial correlation), else 0");
        });
        let mut start = false;
        ui.horizontal(|ui| {
            if running {
                if ui.button("Cancel").clicked()
                    && let Some(j) = &o.job
                {
                    j.cancel.store(true, Ordering::Relaxed);
                }
                if let Some(j) = &o.job {
                    let (f, m) = j.progress.lock().unwrap().clone();
                    ui.add(egui::ProgressBar::new(f as f32).text(m).desired_width(ui.available_width().min(360.0)));
                }
            } else {
                start = ui
                    .button("Run the test")
                    .on_hover_text(
                        "PARTS (Ives et al. 2021): each pixel's AR(1) trend, the spatial correlation of the pixels, \
                         then a GLS of the trends in random partitions of the pixels, combined into tests that \
                         account for the correlation between partitions",
                    )
                    .clicked();
            }
        });
        // run from the command line: once every layer is read (and has its classes, when asked)
        let ready = self.opening.is_none()
            && (0..self.layers.len()).all(|i| self.layer_session(i).is_none_or(|s| s.s.overview.complete()));
        let o = &mut self.analysis.trend;
        if o.auto_run && !running && o.result.is_none() && ready {
            if o.auto_classes {
                o.class_cube = ids.last().copied().unwrap_or(0);
            }
            // without an ROI: what the map shows (--view)
            if !has_roi {
                o.scope = 1;
            }
            start = !o.auto_classes || o.class_cube != 0;
        }
        if start {
            self.start_trend_test(ui.ctx());
        }
        if self.analysis.trend.job.is_some() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
        }
        ui.separator();
        match &self.analysis.trend.result {
            None => {
                ui.weak(
                    "Whether the trend of a region, or of each class, differs from 0, and whether the classes' trends \
                     differ, accounting for the autocorrelation of each series and the spatial correlation of the \
                     pixels (PARTS, remotePARTS). Counting the pixels whose own trend is significant is no such test: \
                     neighbours are correlated, and thousands of tests are run at once.",
                );
            }
            Some(Err(e)) => {
                ui.colored_label(ui.visuals().warn_fg_color, e);
            }
            Some(Ok(r)) => {
                egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| trend_result_ui(ui, r));
            }
        }
    }
}

fn trend_result_ui(ui: &mut Ui, r: &TrendResult) {
    let o = &r.outcome;
    let name = |c: Option<i32>| match c {
        None => "Region".to_string(),
        Some(c) => r.classes.get(&c).map_or_else(|| c.to_string(), |n| n.0.clone()),
    };
    ui.weak(format!(
        "{}{}; {} with a trend, {} tested in {} partitions of {}. Spatial correlation: exp(-d / {} {}); nugget {}. \
         {:.1} s.",
        r.region,
        r.class_layer.as_ref().map_or(String::new(), |l| format!(", classes of {l}")),
        o.fitted,
        o.tested,
        o.npart,
        o.partsize,
        g(o.range),
        r.distance_unit,
        g(o.nugget),
        r.seconds
    ));
    {
        egui::Grid::new("trend test").striped(true).num_columns(8).show(ui, |ui| {
            for head in ["", "Pixels", &format!("Trend ({})", r.unit), "SE", "t", "p", "Pixels up", "Pixels down"] {
                ui.strong(head);
            }
            ui.end_row();
            for m in &o.means {
                ui.horizontal(|ui| {
                    if let Some(c) = m.class.and_then(|c| r.classes.get(&c)) {
                        let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                        ui.painter().rect_filled(rect, 2.0, Color32::from_rgb(c.1[0], c.1[1], c.1[2]));
                    }
                    ui.label(name(m.class));
                });
                ui.label(m.pixels.to_string());
                ui.label(g(m.estimate));
                ui.label(g(m.se));
                ui.label(g(m.t));
                let sig = m.p < 0.05;
                ui.label(RichText::new(g(m.p)).strong().color(if sig {
                    ui.visuals().strong_text_color()
                } else {
                    ui.visuals().weak_text_color()
                }));
                let pct = |k: usize| format!("{k} ({:.0}%)", 100.0 * k as f64 / m.pixels.max(1) as f64);
                ui.label(pct(m.significant_up));
                ui.label(pct(m.significant_down));
                ui.end_row();
            }
        });
    }
    if let Some(p) = o.p_chisqr {
        let p_text = if p <= 1e-6 { "p ≤ 1e-6".to_string() } else { format!("p = {}", g(p)) };
        let verdict =
            if p < 0.05 { "The classes' trends differ" } else { "No evidence that the classes' trends differ" };
        ui.label(format!("{verdict}: {p_text} (F = {}, df {} and {}).", g(o.f), o.df1, o.df2));
    }
    if !o.left_out.is_empty() {
        let list: Vec<String> = o.left_out.iter().map(|&(c, k)| format!("{} ({k} pixels)", name(Some(c)))).collect();
        ui.weak(format!("Left out, too few pixels for every partition: {}.", list.join(", ")));
    }
    ui.horizontal(|ui| {
        if ui.button("Copy CSV").clicked() {
            let mut csv =
                format!("class,pixels,trend{},se,t,p,significant_up,significant_down\n", r.unit.replace('/', "_per_"));
            for m in &o.means {
                csv += &format!(
                    "{},{},{},{},{},{},{},{}\n",
                    name(m.class),
                    m.pixels,
                    m.estimate,
                    m.se,
                    m.t,
                    m.p,
                    m.significant_up,
                    m.significant_down
                );
            }
            if let Some(p) = o.p_chisqr {
                csv += &format!("chi-square p,{p}\n");
            }
            ui.ctx().copy_text(csv);
        }
        ui.weak("The pixels' own tests (AR(1) trend, alpha 0.05) are counted for comparison only.");
    });
}
