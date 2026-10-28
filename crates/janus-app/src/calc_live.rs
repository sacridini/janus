//! The calculator live (phase 39b): its result on the map at the overview's size while the text is typed, a
//! reduction over time shown as its dates are read ("max so far: 23 of 107 dates"); and, in a chart under the
//! editor, the expression's series at the cursor with what its reductions reduce and a part of the text selected.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{Color32, Ui};
use egui_plot::{HLine, Line, Plot, PlotPoints, VLine};
use janus_cube::CubeInfo;
use janus_cube::calc::compile::{Over, RedFn};
use janus_cube::calc::live::{self, LiveUpdate};
use janus_cube::calc::plan::{self, CalcLayer, Key, Options, Plan};

use crate::app::State;
use crate::theme;

/// The text rests this long before the live result or the chart is made again.
const REST: Duration = Duration::from_millis(400);

/// The live result's long side, in pixels.
const LIVE_SIDE: usize = 1024;

/// What a line of the chart shows.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// a value per date
    Series,
    /// one value: a level
    Image,
    /// one value that is a date (argmax, first...): a time
    Date,
}

/// A line of the chart written: its VRTs (a date each) on its grid, their times.
struct ProbeLine {
    name: String,
    kind: Kind,
    files: Vec<String>,
    x: Vec<f64>,
    grid: Arc<CubeInfo>,
}

/// A line of the chart at a pixel.
pub struct ChartLine {
    pub name: String,
    kind: Kind,
    pub pts: Vec<[f64; 2]>,
}

/// The chart's lines read at a pixel.
type Read = ((usize, usize), Vec<ChartLine>);

/// A run in the background: its updates and its stop.
struct Work<T> {
    cancel: Arc<AtomicBool>,
    out: Arc<Mutex<Vec<T>>>,
}

impl<T> Work<T> {
    fn new() -> Self {
        Work { cancel: Arc::new(AtomicBool::new(false)), out: Arc::new(Mutex::new(Vec::new())) }
    }

    fn take(&self) -> Vec<T> {
        std::mem::take(&mut *self.out.lock().unwrap())
    }
}

impl<T> Drop for Work<T> {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

#[derive(Default)]
pub struct CalcLive {
    /// the result on the map as it is typed
    pub on: bool,
    /// the chart under the editor
    pub chart: bool,
    /// what the live result is (or is being made) of, and since when that changed
    key: u64,
    since: Option<Instant>,
    job: Option<Work<Result<LiveUpdate, String>>>,
    /// its layer (a preview result layer) and what it shows
    layer: Option<u32>,
    pub label: String,
    /// the chart's lines written, what of, since when that changed
    probe_key: u64,
    probe_since: Option<Instant>,
    probe_job: Option<Work<Result<Vec<ProbeLine>, String>>>,
    probe: Option<Arc<Vec<ProbeLine>>>,
    probe_dir: Option<PathBuf>,
    /// the lines at the pixel read last (or being read)
    pixel: Option<(usize, usize)>,
    read_job: Option<Work<Read>>,
    pub lines: Vec<ChartLine>,
    pub chart_note: String,
    runs: u64,
}

/// The pixel of grid `g` under pixel (x, y) of `a` (the same layer, or the same CRS by their geotransforms).
fn to_grid(a: &CubeInfo, g: &CubeInfo, x: usize, y: usize) -> Option<(usize, usize)> {
    if a.id == g.id || (a.width, a.height, a.geo_transform) == (g.width, g.height, g.geo_transform) {
        return Some((x, y));
    }
    let (ta, tg) = (a.geo_transform?, g.geo_transform?);
    if a.crs_wkt != g.crs_wkt || tg[1] == 0.0 || tg[5] == 0.0 {
        return None;
    }
    let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
    let (gx, gy) = (ta[0] + px * ta[1] + py * ta[2], ta[3] + px * ta[4] + py * ta[5]);
    let (u, v) = ((gx - tg[0]) / tg[1], (gy - tg[3]) / tg[5]);
    (u >= 0.0 && v >= 0.0 && u < g.width as f64 && v < g.height as f64).then_some((u as usize, v as usize))
}

/// A series' dates as the chart's x: decimal years, or 1, 2, 3...
fn xs(info: &CubeInfo, ts: &[usize]) -> Vec<f64> {
    ts.iter()
        .map(|&t| if info.time_is_date { janus_cube::dates::decimal_year(info.layers[t].time) } else { t as f64 + 1.0 })
        .collect()
}

/// Whether a plan's result is one image that is a date (argmax(ndvi), first(ndvi < 0.3)).
fn is_date_image(p: &Plan) -> bool {
    p.series.is_none()
        && p.program.code.is_empty()
        && matches!(p.inputs.as_slice(), [Key::Stage(j)]
            if matches!(p.stages[*j].f, RedFn::ArgMax | RedFn::ArgMin | RedFn::First | RedFn::Last))
}

/// The lines of the chart written into `dir`: the result, what its reductions reduce, the part selected.
fn write_probe(
    text: &str,
    selected: Option<(String, String)>,
    layers: &[CalcLayer],
    opts: &Options,
    dir: &Path,
    cancel: &AtomicBool,
) -> Result<Vec<ProbeLine>, String> {
    let mut out = Vec::new();
    let line = |name: String, p: &Plan, text: &str, sub: &str| -> Result<ProbeLine, String> {
        let w = p.write(text, layers, opts, &dir.join(sub), &|_, _| {}, cancel)?;
        let kind = match p.series {
            Some(_) => Kind::Series,
            None if is_date_image(p) => Kind::Date,
            None => Kind::Image,
        };
        let x = match p.series {
            Some(s) => {
                let info = p.series_info(layers, s);
                xs(&info, &(0..info.t()).collect::<Vec<_>>())
            }
            None => vec![0.0],
        };
        Ok(ProbeLine { name, kind, files: w.files, x, grid: layers[p.grid].info.clone() })
    };
    let p = plan::plan(text, layers, opts).map_err(|e| e.message)?;
    out.push(line("the result".into(), &p, text, "result")?);
    for j in 0..p.stages.len() {
        let st = &p.stages[j];
        if !matches!(st.over, Over::All) || st.only.is_some() {
            continue;
        }
        let Some(bp) = live::body_plan(&p, j) else { continue };
        let info = p.series_info(layers, st.series);
        let name = format!("what {} reduces", st.f.name());
        let w = bp.write(text, layers, opts, &dir.join(format!("body{}", j + 1)), &|_, _| {}, cancel)?;
        // the dates it reduces (its slice's)
        let ts = live::stage_dates(&p, j, layers);
        out.push(ProbeLine {
            name,
            kind: Kind::Series,
            files: ts.iter().map(|&t| w.files[t].clone()).collect(),
            x: xs(&info, &ts),
            grid: layers[bp.grid].info.clone(),
        });
    }
    if let Some((full, shown)) = selected
        && let Ok(sp) = plan::plan(&full, layers, opts)
    {
        out.push(line(format!("the selection: {shown}"), &sp, &full, "selection")?);
    }
    Ok(out)
}

/// The selection of the editor (its text and that text after the lines before it, which may name what it uses);
/// None without one, or one that is the whole text.
fn selection(ctx: &egui::Context, text: &str) -> Option<(String, String)> {
    let st = egui::TextEdit::load_state(ctx, egui::Id::new(crate::calculator::EDITOR))?;
    let r = st.cursor.char_range()?;
    let (p, s) = (r.primary.index.0, r.secondary.index.0);
    let (a, b) = (p.min(s), p.max(s));
    if a == b {
        return None;
    }
    let byte = |c: usize| text.char_indices().nth(c).map_or(text.len(), |(i, _)| i);
    let (a, b) = (byte(a), byte(b));
    let sel = text[a..b].trim();
    if sel.is_empty() || sel == text.trim() {
        return None;
    }
    let line_start = text[..a].rfind('\n').map_or(0, |i| i + 1);
    Some((format!("{}{sel}", &text[..line_start]), sel.to_string()))
}

impl State {
    /// The live result and the chart follow the text, the options, the date shown and the cursor; called every frame.
    pub(crate) fn pump_calc_live(&mut self, ctx: &egui::Context) {
        let ok = self.calc.show && self.calc.check.as_ref().is_some_and(|c| c.result.is_ok());
        self.pump_live_map(ctx, ok);
        self.pump_live_chart(ctx, ok);
    }

    fn drop_live_layer(&mut self) {
        let l = &mut self.calc.live;
        l.job = None;
        l.label.clear();
        if let Some(id) = l.layer.take() {
            self.zeit.results.retain(|r| r.id != id);
            self.map.dirty = true;
        }
    }

    fn pump_live_map(&mut self, ctx: &egui::Context, ok: bool) {
        if !self.calc.show || !self.calc.live.on || self.calc.text.trim().is_empty() {
            self.drop_live_layer();
            self.calc.live.key = 0;
            return;
        }
        let (layers, at) = self.calc_layers();
        let opts = self.calc_options(&layers, &at);
        let base = self.results_dir().join("calc").join(".live");
        // a series: the date shown
        let time = self
            .session
            .as_ref()
            .filter(|s| s.info.time_is_date)
            .and_then(|s| s.info.layers.get(self.display.t))
            .map(|l| l.time);
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.calc.check.as_ref().map(|c| c.key).hash(&mut h);
        time.map(f64::to_bits).hash(&mut h);
        let key = h.finish();
        let l = &mut self.calc.live;
        if key != l.key {
            l.key = key;
            l.since = Some(Instant::now());
        }
        if ok && l.since.is_some_and(|t| t.elapsed() >= REST) {
            l.since = None;
            if let Ok(p) = plan::plan(&self.calc.text, &layers, &opts) {
                l.runs += 1;
                let dir = base.join(l.runs.to_string());
                let work = Work::new();
                let (cancel, out, c) = (work.cancel.clone(), work.out.clone(), ctx.clone());
                let text = self.calc.text.clone();
                l.label = "computing...".into();
                l.job = Some(work);
                std::thread::spawn(move || {
                    let push = |u| {
                        out.lock().unwrap().push(u);
                        c.request_repaint();
                    };
                    let r = live::live(&p, &text, &layers, &opts, &dir, LIVE_SIDE, time, &cancel, &mut |u| push(Ok(u)));
                    if let Err(e) = r
                        && e != "cancelled"
                    {
                        push(Err(e));
                    }
                    // its values are in memory
                    let _ = std::fs::remove_dir_all(&dir);
                });
            }
        } else if l.since.is_some() {
            ctx.request_repaint_after(REST);
        }
        let updates = l.job.as_ref().map(Work::take).unwrap_or_default();
        for u in updates {
            match u {
                Err(e) => self.calc.live.label = format!("live: {e}"),
                Ok(u) => self.show_live(u),
            }
        }
    }

    /// An update of the live result: its layer made, or its values replaced.
    fn show_live(&mut self, u: LiveUpdate) {
        let (layers, at) = self.calc_layers();
        let opts = self.calc_options(&layers, &at);
        let Ok(p) = plan::plan(&self.calc.text, &layers, &opts) else { return };
        let grid = layers[p.grid].info.clone();
        self.calc.live.label = u.label.clone();
        let name = format!("Live: {}", self.calc.out_name.trim());
        let unit = if is_date_image(&p) { "year".to_string() } else { String::new() };
        if let Some(r) = self.calc.live.layer.and_then(|id| self.zeit.results.iter_mut().find(|r| r.id == id))
            && r.grid.as_ref().is_some_and(|g| g.id == grid.id)
        {
            (r.data, r.tw, r.th, r.name, r.unit) = (u.data, u.w, u.h, name, unit);
            r.auto_range();
            if let Some(gpu) = &self.gpu {
                r.tex = Some(gpu.create_tile(r.tw as u32, r.th as u32, &r.texture_values()));
            }
            self.map.dirty = true;
            return;
        }
        let keep = self.calc.live.job.take();
        self.drop_live_layer();
        self.calc.live.job = keep;
        if self.session.is_none() {
            return;
        }
        let mut r = crate::zeit::ResultLayer::for_test(u.data, u.w, u.h, unit);
        (r.cube, r.grid, r.source, r.name) = (grid.id, Some(grid.clone()), layers[p.grid].name.clone(), name);
        (r.w, r.h, r.visible, r.preview, r.upgrading) = (grid.width, grid.height, true, true, true);
        r.run = "calculator live".into();
        r.auto_range();
        if let Some(gpu) = &self.gpu {
            r.tex = Some(gpu.create_tile(r.tw as u32, r.th as u32, &r.texture_values()));
        }
        self.push_result(r);
        self.calc.live.layer = Some(self.zeit.next_result);
        self.map.dirty = true;
    }

    fn pump_live_chart(&mut self, ctx: &egui::Context, ok: bool) {
        if !self.calc.show || !self.calc.live.chart {
            let l = &mut self.calc.live;
            (l.probe_job, l.read_job, l.probe_key) = (None, None, 0);
            return;
        }
        let (layers, at) = self.calc_layers();
        let opts = self.calc_options(&layers, &at);
        let selected = selection(ctx, &self.calc.text);
        let base = self.results_dir().join("calc").join(".probe");
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.calc.check.as_ref().map(|c| c.key).hash(&mut h);
        selected.hash(&mut h);
        let key = h.finish();
        let l = &mut self.calc.live;
        if key != l.probe_key {
            l.probe_key = key;
            l.probe_since = Some(Instant::now());
        }
        if ok && l.probe_since.is_some_and(|t| t.elapsed() >= REST) {
            l.probe_since = None;
            l.runs += 1;
            let dir = base.join(l.runs.to_string());
            let work = Work::new();
            let (cancel, out, c) = (work.cancel.clone(), work.out.clone(), ctx.clone());
            let text = self.calc.text.clone();
            l.probe_job = Some(work);
            l.chart_note = "writing...".into();
            std::thread::spawn(move || {
                let r = write_probe(&text, selected, &layers, &opts, &dir, &cancel).inspect_err(|_| {
                    let _ = std::fs::remove_dir_all(&dir);
                });
                out.lock().unwrap().push(r);
                c.request_repaint();
            });
            // the folder of the lines written before, gone once its reads end
            if let Some(old) = l.probe_dir.take() {
                std::thread::spawn(move || {
                    for _ in 0..50 {
                        std::thread::sleep(Duration::from_millis(200));
                        if std::fs::remove_dir_all(&old).is_ok() || !old.exists() {
                            return;
                        }
                    }
                });
            }
            l.probe_dir = Some(base.join(l.runs.to_string()));
        } else if l.probe_since.is_some() {
            ctx.request_repaint_after(REST);
        }
        let done = l.probe_job.as_ref().map(Work::take).unwrap_or_default();
        for r in done {
            match r {
                Ok(lines) => {
                    l.probe = Some(Arc::new(lines));
                    l.pixel = None;
                    l.chart_note.clear();
                }
                Err(e) => {
                    l.probe = None;
                    l.lines.clear();
                    l.chart_note = e;
                }
            }
        }
        // the cursor's pixel (else the first pin's), read when it changed and no read is under way
        let px = self.series.hover.as_ref().or(self.series.pins.first()).map(|v| (v.x, v.y));
        let l = &mut self.calc.live;
        let read = l.read_job.as_ref().map(Work::take).unwrap_or_default();
        if !read.is_empty() {
            l.read_job = None;
        }
        for (at, lines) in read {
            if Some(at) == l.pixel {
                l.lines = lines;
            }
        }
        let (Some(probe), Some(px), Some(s)) = (l.probe.clone(), px, self.session.as_ref()) else { return };
        if l.read_job.is_some() || l.pixel == Some(px) {
            return;
        }
        l.pixel = Some(px);
        let active = s.info.clone();
        let work = Work::new();
        let (out, c) = (work.out.clone(), ctx.clone());
        l.read_job = Some(work);
        std::thread::spawn(move || {
            let lines = probe
                .iter()
                .filter_map(|pl| {
                    let (x, y) = to_grid(&active, &pl.grid, px.0, px.1)?;
                    let v = live::read_pixel(&pl.files, x, y);
                    let pts = pl.x.iter().zip(v).filter(|(_, v)| v.is_finite()).map(|(&x, v)| [x, v as f64]).collect();
                    Some(ChartLine { name: pl.name.clone(), kind: pl.kind, pts })
                })
                .collect();
            out.lock().unwrap().push((px, lines));
            c.request_repaint();
        });
    }

    /// The chart under the editor: the result's series at the cursor (a level for an image, a time for a date),
    /// what its reductions reduce, the part of the text selected.
    pub(crate) fn calc_chart_ui(&mut self, ui: &mut Ui) {
        let l = &self.calc.live;
        if !l.chart_note.is_empty() {
            ui.weak(&l.chart_note);
        }
        if l.lines.is_empty() {
            ui.weak("Move the cursor over the map: the expression's values there (select a part of the text to see it too).");
            return;
        }
        let colours = [theme::accent(), Color32::from_rgb(150, 150, 150), Color32::from_rgb(230, 120, 60)];
        Plot::new("calc chart").height(170.0).legend(egui_plot::Legend::default()).show(ui, |pu| {
            for (i, cl) in l.lines.iter().enumerate() {
                let col =
                    theme::on_plot(colours[(i.min(1) + usize::from(cl.name.starts_with("the selection"))).min(2)]);
                match (cl.kind, cl.pts.first()) {
                    (Kind::Series, _) => {
                        pu.line(Line::new(cl.name.as_str(), PlotPoints::from(cl.pts.clone())).color(col).width(2.0))
                    }
                    (Kind::Image, Some(p)) => pu.hline(HLine::new(cl.name.as_str(), p[1]).color(col).width(1.5)),
                    (Kind::Date, Some(p)) => pu.vline(VLine::new(cl.name.as_str(), p[1]).color(col).width(1.5)),
                    _ => {}
                }
            }
        });
    }

    /// The lines of the chart now, as text (the tests).
    pub(crate) fn calc_chart_text(&self) -> String {
        self.calc
            .live
            .lines
            .iter()
            .map(|l| format!("{}: {} values", l.name, l.pts.len()))
            .collect::<Vec<_>>()
            .join("; ")
    }
}
