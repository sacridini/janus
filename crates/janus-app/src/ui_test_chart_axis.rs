//! `--selftest-ui` stages 350-354: the time series chart's second Y axis (chart_axis.rs, chart.rs). 350 and 351 run the
//! calculator on the first series: `A * 300` (a series of the range 300 times as wide) and `corr(A, A * 300, lag=2)`
//! (a result whose provenance names its lag); 352 puts the first on the right axis and checks the axes' marks (the
//! left in A's range, the right in the other's) and the figure's SVG (the right axis' numbers, the legend's "(right)");
//! 353 draws it as bars; 354 shifts A by the lag (the menu offers it, the legend says it, the figure changes) and puts
//! everything back. Then 299, the end.

use std::sync::Mutex;
use std::time::Instant;

use super::UiTest;
use crate::app::State;
use crate::chart_axis::{Lag, Look};
use crate::figure::{FigureOptions, Format, Rendered};
use crate::figure_ui::Source;

const RAIN: &str = "Axis rain";
const LAGGED: &str = "Axis lag";

/// What the stages share: the layers before, the chart's layers option and the cursor, the first figure.
#[derive(Default)]
struct Kept {
    layers: usize,
    chart_layers: i32,
    cursor_put: bool,
    lines_png: Vec<u8>,
    lines_svg: String,
}

static KEPT: Mutex<Option<Kept>> = Mutex::new(None);

fn with_kept<R>(f: impl FnOnce(&mut Kept) -> R) -> R {
    let mut g = KEPT.lock().unwrap();
    f(g.get_or_insert_with(Kept::default))
}

/// The texts of an SVG, in order.
fn svg_texts(svg: &str) -> Vec<String> {
    svg.split("<text ")
        .skip(1)
        .filter_map(|t| {
            let from = t.find('>')? + 1;
            let to = t.find("</text>")?;
            Some(t[from..to].trim().to_string())
        })
        .collect()
}

fn figure(o: &FigureOptions, s: &mut State) -> Option<Rendered> {
    s.render_figure(Source::Chart, o)
}

impl State {
    pub(super) fn test_chart_axis(&mut self, st: &mut UiTest, t0: Instant, ctx: &egui::Context) -> Option<i32> {
        let stage = st.stage;
        let fail = |what: &str| {
            println!("FAIL (stage {stage}): {what}");
            Some(1)
        };
        let next = |st: &mut UiTest, done: &str| {
            println!("[{:5.1} s] {done}", t0.elapsed().as_secs_f64());
            st.stage += 1;
            st.since = Some(Instant::now());
            st.frames = 0;
        };
        let waited = st.since.map_or(0.0, |s| s.elapsed().as_secs_f64());
        self.session.as_ref()?;
        let layer_of = |s: &State, name: &str| s.layers.iter().position(|l| l.name == name);
        let (w, h) = self.session.as_ref().map_or((0, 0), |s| (s.info.width, s.info.height));
        match stage {
            350 | 351 => {
                let name = if stage == 350 { RAIN } else { LAGGED };
                if st.frames == 0 {
                    // the series layer the expression is written on: the active one, or the first of several dates
                    let (ls, at) = self.calc_layers();
                    let series = |k: usize| ls[k].info.t() > 1;
                    let active = at.iter().position(|&i| i == self.active).filter(|&k| series(k));
                    let Some(k) = active.or_else(|| (0..ls.len()).find(|&k| series(k))) else {
                        println!("    no series open: the second axis is left out");
                        st.stage = 299;
                        return None;
                    };
                    if stage == 350 {
                        with_kept(|k| k.layers = self.layers.len());
                    }
                    let a = ls[k].alias.clone();
                    self.calc.text =
                        if stage == 350 { format!("{a} * 300") } else { format!("corr({a}, {a} * 300, lag=2)") };
                    self.calc.out_name = name.into();
                    self.calc.cog = false;
                    self.calc_run(ctx);
                    st.frames = 1;
                    return None;
                }
                let Some(k) = layer_of(self, name) else {
                    if self
                        .emb_downloads
                        .iter()
                        .any(|d| d.status.lock().unwrap().state == crate::zeit::JobState::Failed)
                    {
                        return fail("the calculator failed (Tools > Tasks > Log)");
                    }
                    return if waited > 120.0 { fail("the calculator's layer should open") } else { None };
                };
                if self.opening.is_some() || !super::loaded(self.layer_session(k)) {
                    return None;
                }
                let t = self.layer_session(k)?.info.t();
                println!("    \"{name}\": {t} date{}", if t == 1 { "" } else { "s" });
                next(
                    st,
                    if stage == 350 {
                        "the calculator's A * 300 is a layer"
                    } else {
                        "the calculator's corr(A, A * 300, lag=2) is a layer"
                    },
                );
            }
            352 => {
                let Some(rain) = layer_of(self, RAIN) else { return fail("the A * 300 layer should be open") };
                let info = self.session.as_ref()?.info.clone();
                let n = info.t();
                if st.frames == 0 {
                    with_kept(|k| {
                        k.chart_layers = self.series.chart_layers;
                        k.cursor_put = self.series.hover.is_none() && self.series.pins.is_empty();
                    });
                    if self.series.hover.is_none() && self.series.pins.is_empty() {
                        self.set_hover(Some((w / 2, h / 2)));
                    }
                    self.set_chart_layers(1);
                    st.frames = 1;
                    return None;
                }
                // the cursor's series of both layers read
                let ready = self.series.hover.as_ref().is_some_and(|h| h.values.len() == n)
                    && self.layers[rain].series.hover.as_ref().is_some_and(|h| h.values.len() == n && h.exact);
                if !ready {
                    return if waited > 60.0 { fail("both layers' series should be read at the cursor") } else { None };
                }
                let active = self.layers[self.active].name.clone();
                let xs: Vec<f64> = info.layers.iter().map(|l| l.time).collect();
                let exts = self.layer_extents(&self.chart, &xs, info.time_is_date, info.spectral, false);
                let find = |name: &str| exts.iter().find(|e| e.0 == name).map(|e| e.1);
                let (Some(a), Some(r)) = (find(&active), find(RAIN)) else {
                    return fail("both layers should have a range");
                };
                println!("    ranges: {active} {:.3} to {:.3}, {RAIN} {:.3} to {:.3}", a.0, a.1, r.0, r.1);
                if (r.1 - 300.0 * a.1).abs() > 1e-3 * r.1.abs().max(1.0) || a.1.abs() * 300.0 >= 1500.0 {
                    return fail("the second layer's range should be 300 times the first's (and under 1500)");
                }
                // one axis: nothing on the right
                if self.dual_axes(&self.chart, &exts, 300.0).is_some() {
                    return fail("with nothing on the right there is no second axis");
                }
                let o = FigureOptions {
                    width_mm: 180.0,
                    height_mm: 110.0,
                    dpi: 150,
                    format: Format::Svg,
                    ..Default::default()
                };
                let plain = figure(&o, self)?;
                let (plain_svg, plain_png) = (plain.to_svg(180.0, 110.0), plain.to_png().ok()?);
                self.chart.looks.insert(RAIN.into(), Look { right: true, ..Default::default() });
                let Some(d) = self.dual_axes(&self.chart, &exts, 300.0) else {
                    return fail("a layer on the right should give a second axis");
                };
                // the left covers the first layer's range, the right the second's, mark for mark
                if d.left.start > a.0 || d.left.end() < a.1 || d.right.start > r.0 || d.right.end() < r.1 {
                    return fail("the axes' marks should cover their layers' ranges");
                }
                if d.left.n != d.right.n {
                    return fail("both axes should have the same number of intervals");
                }
                let marks = d
                    .left
                    .values()
                    .iter()
                    .zip(d.right.values())
                    .all(|(l, r)| (d.map.inv(*l) - r).abs() < 1e-9 * r.abs().max(1.0));
                if !marks {
                    return fail("a mark of the left axis should be a mark of the right");
                }
                println!(
                    "    left {} to {} by {}, right {} to {} by {} ({} intervals)",
                    d.left.start,
                    d.left.end(),
                    d.left.step,
                    d.right.start,
                    d.right.end(),
                    d.right.step,
                    d.left.n
                );
                let fig = figure(&o, self)?;
                let svg = fig.to_svg(180.0, 110.0);
                let texts = svg_texts(&svg);
                let nums: Vec<f64> = texts.iter().filter_map(|t| t.parse().ok()).collect();
                // numbers beyond the left axis' range (and not years): the right axis' marks
                let right: Vec<f64> = nums.iter().copied().filter(|v| *v > a.1 * 1.5 + 1.0 && *v < 1500.0).collect();
                let left: Vec<f64> =
                    nums.iter().copied().filter(|v| *v >= a.0 - 1e-9 && *v <= d.left.end() + 1e-9).collect();
                println!("    the SVG's numbers: {} on the right ({right:?}), {} on the left", right.len(), left.len());
                if right.len() < 2 || right.iter().cloned().fold(0.0, f64::max) < r.1 * 0.5 {
                    return fail("the figure should draw the right axis' numbers (up to the second layer's range)");
                }
                if left.len() < 2 {
                    return fail("the figure should draw the left axis' numbers");
                }
                if !texts.iter().any(|t| t.contains("(right)")) {
                    return fail("the legend should say which series are on the right axis");
                }
                if svg == plain_svg {
                    return fail("the figure should change with the right axis");
                }
                // the figure's right margin has the axis' numbers: ink there with it
                let img = fig.to_png().ok()?;
                let ink = |png: &[u8]| {
                    let im = image::load_from_memory(png).map(|i| i.to_rgba8()).ok();
                    im.map_or(0, |im| {
                        let from = im.width() * 93 / 100;
                        im.enumerate_pixels().filter(|(x, _, p)| *x >= from && p.0[3] > 0 && p.0[0] < 180).count()
                    })
                };
                let (with, without) = (ink(&img), ink(&plain_png));
                println!("    ink in the right margin: {with} px with the axis, {without} px without");
                if with < 100 {
                    return fail("the right axis should put its numbers in the figure's right margin");
                }
                with_kept(|k| {
                    k.lines_svg = svg;
                    k.lines_png = img;
                });
                next(st, "the right axis: marks in both layers' ranges, its numbers and the legend in the figure");
            }
            353 => {
                // the second layer as bars
                let o = FigureOptions {
                    width_mm: 180.0,
                    height_mm: 110.0,
                    dpi: 150,
                    format: Format::Svg,
                    ..Default::default()
                };
                self.chart.looks.entry(RAIN.into()).or_default().bars = true;
                let fig = figure(&o, self)?;
                let (png, svg) = (fig.to_png().ok()?, fig.to_svg(180.0, 110.0));
                let (lines_png, lines_svg) = with_kept(|k| (k.lines_png.clone(), k.lines_svg.clone()));
                let (a, b) = (
                    image::load_from_memory(&lines_png).ok()?.to_rgba8(),
                    image::load_from_memory(&png).ok()?.to_rgba8(),
                );
                let differ = a.pixels().zip(b.pixels()).filter(|(p, q)| p != q).count();
                let filled = |s: &str| s.matches("<path ").count() + s.matches("<rect ").count();
                println!(
                    "    bars: {differ} pixels differ from the lines, {} shapes against {}",
                    filled(&svg),
                    filled(&lines_svg)
                );
                if a.dimensions() != b.dimensions() || differ < 300 {
                    return fail("the bars should draw differently from the lines");
                }
                if !svg_texts(&svg).iter().any(|t| t.contains("(right)")) {
                    return fail("the bars' legend should still say they are on the right");
                }
                // a copy for whoever wants to look at it
                if let Ok(keep) = std::env::var("JANUS_AXIS_KEEP") {
                    let _ = std::fs::create_dir_all(&keep);
                    let dir = std::path::Path::new(&keep);
                    let _ = std::fs::write(dir.join("right_axis_lines.png"), &lines_png);
                    let _ = std::fs::write(dir.join("right_axis_lines.svg"), &lines_svg);
                    let _ = std::fs::write(dir.join("right_axis_bars.png"), &png);
                }
                next(st, "the second layer as bars on the right axis");
            }
            354 => {
                if layer_of(self, LAGGED).is_none() {
                    return fail("the corr layer should be open");
                }
                let info = self.session.as_ref()?.info.clone();
                let active = self.layers[self.active].name.clone();
                // the menu offers the shift of A: its name, the lag of the corr
                let (_, lags) = self.axes_menu_rows(&self.chart, &info, false);
                let want = (LAGGED.to_string(), active.clone(), Lag::Obs(2));
                println!("    the menu's lags: {lags:?}");
                if !lags.contains(&want) {
                    return if waited > 10.0 {
                        fail("the menu should offer to shift A by the corr's lag")
                    } else {
                        None
                    };
                }
                let o = FigureOptions {
                    width_mm: 180.0,
                    height_mm: 110.0,
                    dpi: 150,
                    format: Format::Svg,
                    ..Default::default()
                };
                let before = figure(&o, self)?.to_svg(180.0, 110.0);
                self.chart.looks.entry(active.clone()).or_default().shift = Some(Lag::Obs(2));
                let after = figure(&o, self)?.to_svg(180.0, 110.0);
                if before == after {
                    return fail("the shifted series should be drawn elsewhere");
                }
                if !svg_texts(&after).iter().any(|t| t.contains("(shifted +2 obs)")) {
                    return fail("the legend should say the series is shifted");
                }
                println!("    shifted by 2 observations: the figure changes and the legend says it");
                // everything back: the layers, the chart's options, the cursor
                self.chart.looks.clear();
                self.chart.y_from_map_right = false;
                let (before_layers, chart_layers, cursor_put) = with_kept(|k| (k.layers, k.chart_layers, k.cursor_put));
                for i in (before_layers..self.layers.len()).rev() {
                    self.remove_layer(i, ctx);
                }
                self.set_chart_layers(chart_layers);
                if cursor_put {
                    self.series.hover = None;
                }
                *KEPT.lock().unwrap() = None;
                next(st, "A shifted by the corr's lag; the chart put back");
                st.stage = 299;
            }
            _ => {}
        }
        None
    }
}
