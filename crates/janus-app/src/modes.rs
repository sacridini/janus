//! Analysis > Modes: what the temporal EOF and the lagged correlation (Tools > Temporal dynamics) say beyond their maps.
//!
//! An EOF run: the scree plot (the share of the variance of each mode with North's error bars, a mode that is
//! separable from its neighbours filled, one that is mixed with them hollow, North et al. 1982), the principal
//! components over the dates, a table, and the buttons that show a mode's map and copy or save the components. A mode's
//! map on top of the others also draws the mode at the chart's pixel (`draw_eof_overlay`). A lagged-correlation run: r
//! against the lag at the cursor or a pin with the level a correlation must pass there, the share of pixels that pass
//! the false discovery rate and the lags of those that do.
//!
//! The run's numbers come from what the tool wrote (eof.json, pcs.csv and the maps), not from a computation here.

use std::path::Path;
use std::sync::Arc;

use egui::{Color32, RichText, Ui};
use egui_plot::{Bar, BarChart, HLine, Line, LineStyle, Plot, PlotPoints, PlotUi, Points, VLine};
use janus_zeit::Params;
use serde_json::Value;

use crate::app::State;
use crate::theme;

/// The tab's number in Analysis (--analysis modes).
pub(crate) const TAB: usize = 11;

/// The colours of the components, one for each mode (Tableau 10).
const COLOURS: [[u8; 3]; 10] = [
    [31, 119, 180],
    [255, 127, 14],
    [44, 160, 44],
    [214, 39, 40],
    [148, 103, 189],
    [140, 86, 75],
    [227, 119, 194],
    [127, 127, 127],
    [188, 189, 34],
    [23, 190, 207],
];

fn colour(k: usize) -> Color32 {
    let [r, g, b] = COLOURS[k % COLOURS.len()];
    theme::on_plot(Color32::from_rgb(r, g, b))
}

/// What a map of an EOF run is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModeKind {
    /// the pattern: the data's units per standard deviation of the component
    Eof,
    /// the pattern of a varimax rotation of the first modes
    Rotated,
    /// the correlation of each pixel with the component
    Corr,
}

/// A mode's map (an EOF run's output) with the numbers that go with it, read from the run's describe.
#[derive(Clone, Debug)]
pub(crate) struct EofMode {
    pub mode: usize,
    pub kind: ModeKind,
    /// the share of the total variance
    pub variance: f64,
    /// North's error as a share of the total variance, and whether the mode is separable (not for the others)
    pub north_error: Option<f64>,
    pub separable: Option<bool>,
    /// the component over the series' dates (NaN: not defined at that date)
    pub pc: Vec<f64>,
    /// the run's parameters (its anomalies are what the chart's overlay adds to the pattern)
    pub params: Arc<Params>,
}

impl EofMode {
    /// The mode an output of an EOF run describes (None for another output).
    pub(crate) fn from_output(o: &Value, params: &Arc<Params>) -> Option<EofMode> {
        let id = o["id"].as_str()?;
        let kind = if id.starts_with("eof_rotated_") {
            ModeKind::Rotated
        } else if id.starts_with("eof_") {
            ModeKind::Eof
        } else if id.starts_with("corrmap_") {
            ModeKind::Corr
        } else {
            return None;
        };
        let numbers = |v: &Value| -> Vec<f64> {
            v.as_array().map(|a| a.iter().map(|x| x.as_f64().unwrap_or(f64::NAN)).collect()).unwrap_or_default()
        };
        Some(EofMode {
            mode: o["mode"].as_u64()? as usize,
            kind,
            variance: o["variance"].as_f64().unwrap_or(f64::NAN),
            north_error: o["north_error"].as_f64(),
            separable: o["separable"].as_bool(),
            pc: numbers(&o["amplitude"]["y"]),
            params: params.clone(),
        })
    }

    /// "EOF 2" (the chart's overlay's name).
    pub(crate) fn label(&self) -> String {
        match self.kind {
            ModeKind::Rotated => format!("Rotated EOF {}", self.mode),
            _ => format!("EOF {}", self.mode),
        }
    }
}

/// A run's numbers of every mode (eof.json) and components (pcs.csv).
#[derive(Clone, Debug, Default)]
pub(crate) struct EofRun {
    pub modes: usize,
    pub dates: usize,
    pub pixels: usize,
    pub solver: String,
    pub weights: String,
    pub north_n: String,
    /// by mode: the share of the variance, North's error (a share of the total variance), separable
    pub variance: Vec<f64>,
    pub delta: Vec<f64>,
    pub separable: Vec<bool>,
    /// the shares of the rotated modes (empty without a rotation)
    pub rotated: Vec<f64>,
    pub labels: Vec<String>,
    pub years: Vec<f64>,
    /// the dates left out of the fit, their components projected
    pub filled: Vec<bool>,
    /// the components by mode, over the dates; the rotated ones
    pub pcs: Vec<Vec<f64>>,
    pub rotated_pcs: Vec<Vec<f64>>,
    pub csv: String,
}

fn numbers(v: &Value) -> Vec<f64> {
    v.as_array().map(|a| a.iter().map(|x| x.as_f64().unwrap_or(f64::NAN)).collect()).unwrap_or_default()
}

impl EofRun {
    /// eof.json and pcs.csv of a run's folder.
    pub(crate) fn read(dir: &Path) -> Result<EofRun, String> {
        let text = |name: &str| std::fs::read_to_string(dir.join(name)).map_err(|e| format!("{name}: {e}"));
        let doc: Value = serde_json::from_str(&text("eof.json")?).map_err(|e| format!("eof.json: {e}"))?;
        let csv = text("pcs.csv")?;
        let total = doc["total_variance"].as_f64().filter(|t| *t > 0.0).unwrap_or(1.0);
        let mut run = EofRun {
            modes: doc["modes"].as_u64().unwrap_or(0) as usize,
            dates: doc["dates"].as_u64().unwrap_or(0) as usize,
            pixels: doc["n_pixels"].as_u64().unwrap_or(0) as usize,
            solver: doc["solver"]["name"].as_str().unwrap_or("").to_string(),
            weights: doc["weights"].as_str().unwrap_or("").to_string(),
            north_n: doc["north_n"].as_str().unwrap_or("").to_string(),
            variance: numbers(&doc["variance"]),
            delta: numbers(&doc["north_error"]).iter().map(|d| d / total).collect(),
            separable: doc["separable"].as_array().map_or(vec![], |a| a.iter().map(|b| b == true).collect()),
            rotated: numbers(&doc["variance_rotated"]),
            csv: csv.clone(),
            ..Default::default()
        };
        let mut lines = csv.lines();
        let header: Vec<&str> = lines.next().unwrap_or("").split(',').collect();
        let cols = |prefix: &str| -> Vec<usize> {
            header.iter().enumerate().filter(|(_, h)| h.starts_with(prefix)).map(|(i, _)| i).collect()
        };
        let (pc_cols, rot_cols) = (cols("pc"), cols("rotated"));
        run.pcs = vec![vec![]; pc_cols.len()];
        run.rotated_pcs = vec![vec![]; rot_cols.len()];
        for line in lines.filter(|l| !l.is_empty()) {
            let f: Vec<&str> = line.split(',').collect();
            let num = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(f64::NAN);
            run.labels.push(f.first().unwrap_or(&"").to_string());
            run.years.push(num(1));
            run.filled.push(f.get(2) == Some(&"1"));
            for (k, &c) in pc_cols.iter().enumerate() {
                run.pcs[k].push(num(c));
            }
            for (k, &c) in rot_cols.iter().enumerate() {
                run.rotated_pcs[k].push(num(c));
            }
        }
        run.modes = run.modes.max(run.pcs.len());
        Ok(run)
    }
}

/// A lagged-correlation run's numbers: the lags, and the false discovery rate's threshold.
#[derive(Clone, Debug, Default)]
pub(crate) struct CorrRun {
    pub lags: Vec<i32>,
    /// "months" or "dates"
    pub unit: String,
    /// the file of r at every lag (a band a lag), if the run wrote it
    pub r_lags: Option<String>,
    /// the false discovery rate's p-value threshold (of the best lag's adjusted p-value) and its alpha
    pub p_star: f64,
    pub alpha: f64,
    /// the best lag's p-value was adjusted for the lags tried (Sidak)
    pub sidak: bool,
}

/// An EOF or lagged-correlation run the Modes tab can show.
#[derive(Clone, Debug)]
pub(crate) struct ModesRun {
    pub run: String,
    /// the series (layer) it ran on, its window in that series' pixels
    pub cube: u64,
    pub window: [usize; 4],
    pub title: String,
    /// each map written: the output's id ("eof_2", "best_r") and its file
    pub maps: Vec<(String, String)>,
    pub eof: Option<Arc<EofRun>>,
    pub corr: Option<CorrRun>,
}

impl ModesRun {
    /// The run `tool` made into `dir`, from what it described (None for another tool).
    pub(crate) fn new(
        tool: &str,
        dir: &str,
        cube: u64,
        window: [usize; 4],
        title: &str,
        outputs: &[Value],
        params: &Params,
    ) -> Option<ModesRun> {
        let maps = outputs
            .iter()
            .filter(|o| o["series"].as_bool() != Some(true) && o["vector"].as_bool() != Some(true))
            .filter_map(|o| Some((o["id"].as_str()?.to_string(), o["path"].as_str()?.to_string())))
            .collect();
        let (mut eof, mut corr) = (None, None);
        match tool {
            "eof" => eof = Some(Arc::new(EofRun::read(Path::new(dir)).ok()?)),
            "lagged_correlation" => {
                let by = |id: &str| outputs.iter().find(|o| o["id"].as_str() == Some(id));
                let lags = by("r_lags");
                let q = by("q");
                corr = Some(CorrRun {
                    lags: lags
                        .and_then(|o| o["lags"].as_array())
                        .map(|a| a.iter().filter_map(|k| k.as_i64().map(|k| k as i32)).collect())
                        .unwrap_or_default(),
                    unit: by("best_lag").and_then(|o| o["unit"].as_str()).unwrap_or("months").to_string(),
                    r_lags: lags.and_then(|o| o["path"].as_str()).map(str::to_string),
                    p_star: q.and_then(|o| o["p_threshold"].as_f64()).unwrap_or(f64::NAN),
                    alpha: q.and_then(|o| o["alpha_fdr"].as_f64()).unwrap_or(f64::NAN),
                    sidak: params.bool("sidak"),
                });
            }
            _ => return None,
        }
        Some(ModesRun {
            run: dir.to_string(),
            cube,
            window,
            title: title.to_string(),
            maps,
            eof,
            corr,
        })
    }

    fn path_of(&self, id: &str) -> Option<&str> {
        self.maps.iter().find(|(i, _)| i == id).map(|(_, p)| p.as_str())
    }
}

/// The parameters a run's job.json holds over the tool's defaults.
pub(crate) fn run_params(tool: &str, dir: &str) -> Arc<Params> {
    let given = std::fs::read_to_string(Path::new(dir).join("job.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("params").cloned())
        .and_then(|p| serde_json::from_value(p).ok())
        .unwrap_or_default();
    Arc::new(Params::with_defaults(tool, &given).unwrap_or_default())
}

/// The sentence about North's test: the runs of modes that are mixed with a neighbour.
pub(crate) fn mixed_text(separable: &[bool]) -> String {
    let mut groups: Vec<Vec<usize>> = vec![];
    for (i, &s) in separable.iter().enumerate() {
        if !s {
            match groups.last_mut() {
                Some(g) if g.last() == Some(&i) => g.push(i + 1),
                _ => groups.push(vec![i + 1]),
            }
        }
    }
    if groups.is_empty() {
        return "Every mode is separable from its neighbours (North et al. 1982).".into();
    }
    let list = |g: &[usize]| match g {
        [a] => format!("{a}"),
        [a, b] => format!("{a} and {b}"),
        _ => format!("{} and {}", g[..g.len() - 1].iter().map(usize::to_string).collect::<Vec<_>>().join(", "), g[g.len() - 1]),
    };
    let parts: Vec<String> = groups
        .iter()
        .map(|g| {
            if g.len() == 1 {
                format!("mode {} is mixed with its neighbour", list(g))
            } else {
                format!("modes {} are mixed", list(g))
            }
        })
        .collect();
    let mut text = parts.join("; ");
    text = text[..1].to_uppercase() + &text[1..];
    format!("{text} (North et al. 1982): read them together or rotate.")
}

/// A component in standard deviations (its mean taken out).
fn standardized(pc: &[f64]) -> Vec<f64> {
    let v: Vec<f64> = pc.iter().copied().filter(|x| x.is_finite()).collect();
    if v.len() < 2 {
        return pc.to_vec();
    }
    let mean = v.iter().sum::<f64>() / v.len() as f64;
    let sd = (v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (v.len() - 1) as f64).sqrt();
    pc.iter().map(|x| if sd > 0.0 { (x - mean) / sd } else { x - mean }).collect()
}

/// The Modes tab's state.
#[derive(Default)]
pub struct ModesUi {
    /// the run shown (its folder)
    pub run: String,
    /// the components drawn, by mode (empty: the first three)
    pub on: Vec<bool>,
    /// the rotated components and maps instead
    pub rotated: bool,
    /// the pixel of the correlation's lags: "cursor" or "pin<id>"
    source: String,
    /// r at every lag of a pixel: the file, the pixel and the values
    pub(crate) lags_at: Option<(String, usize, usize, Vec<f64>)>,
    /// the share of significant pixels and the histogram of their best lags, for a run and a map's size
    summary: Option<((String, usize), LagSummary)>,
    /// frames the tab was drawn with a run (the self-test)
    pub drawn: u64,
}

impl ModesUi {
    /// The share of the pixels that are significant, once the maps are read.
    pub(crate) fn share_significant(&self) -> Option<f64> {
        self.summary.as_ref().filter(|s| s.1.valid > 0).map(|s| s.1.significant as f64 / s.1.valid as f64)
    }
}

/// The significant pixels of a lagged-correlation run, from its maps.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct LagSummary {
    pub valid: usize,
    pub significant: usize,
    /// the best lag and the pixels significant with it
    pub hist: Vec<(i32, usize)>,
}

/// The share of significant pixels and the best lags of those (`significant` is 2 where a pixel passes, 1 where not).
pub(crate) fn lag_summary(significant: &[f32], best_lag: &[f32]) -> LagSummary {
    let mut out = LagSummary::default();
    let mut hist: std::collections::BTreeMap<i32, usize> = Default::default();
    for (s, l) in significant.iter().zip(best_lag) {
        if !s.is_finite() {
            continue;
        }
        out.valid += 1;
        if s.round() as i32 == 2 && l.is_finite() {
            out.significant += 1;
            *hist.entry(l.round() as i32).or_default() += 1;
        }
    }
    out.hist = hist.into_iter().collect();
    out
}

/// r at every lag of a pixel of the run's window, from the file of the lags (a band a lag).
pub(crate) fn read_lags(path: &str, x: usize, y: usize) -> Result<Vec<f64>, String> {
    let ds = gdal::Dataset::open(path).map_err(|e| format!("{path}: {e}"))?;
    (1..=ds.raster_count())
        .map(|b| {
            let band = ds.rasterband(b).map_err(|e| e.to_string())?;
            let nd = band.no_data_value().filter(|v| !v.is_nan());
            let buf = band.read_as::<f32>((x as isize, y as isize), (1, 1), (1, 1), None).map_err(|e| e.to_string())?;
            let v = buf.data().first().copied().unwrap_or(f32::NAN);
            Ok(if nd.is_some_and(|n| v == n as f32) { f64::NAN } else { f64::from(v) })
        })
        .collect()
}

impl State {
    /// The runs the tab can show: of the active series, with a map still open.
    pub(crate) fn modes_runs(&self) -> Vec<&ModesRun> {
        let Some(s) = &self.session else { return vec![] };
        self.zeit
            .modes_runs
            .iter()
            .filter(|m| m.cube == s.info.id && self.zeit.results.iter().any(|r| r.run == m.run))
            .collect()
    }

    /// The run shown: the one chosen, else the latest.
    pub(crate) fn modes_current(&self) -> Option<ModesRun> {
        let runs = self.modes_runs();
        runs.iter().find(|m| m.run == self.analysis.modes.run).or(runs.last()).map(|m| (*m).clone())
    }

    /// Shows a mode's map (the others of its kind hidden: the one on top is what the chart's overlay follows).
    /// Returns false when the map was not written (`maps` modes are).
    pub(crate) fn modes_show_map(&mut self, run: &str, mode: usize, rotated: bool) -> bool {
        let Some(r) = self.zeit.modes_runs.iter().find(|m| m.run == run) else { return false };
        let id = if rotated { format!("eof_rotated_{mode}") } else { format!("eof_{mode}") };
        let Some(path) = r.path_of(&id).map(str::to_string) else { return false };
        let siblings: Vec<String> = r
            .maps
            .iter()
            .filter(|(i, _)| i.starts_with("eof_"))
            .map(|(_, p)| p.clone())
            .collect();
        let mut found = false;
        for l in self.zeit.results.iter_mut().filter(|l| l.run == run) {
            if l.path == path {
                l.visible = true;
                found = true;
            } else if siblings.contains(&l.path) {
                l.visible = false;
            }
        }
        self.map.dirty = true;
        found
    }

    /// The components as CSV text (pcs.csv).
    pub(crate) fn modes_pcs_csv(&self, run: &str) -> Option<String> {
        self.zeit.modes_runs.iter().find(|m| m.run == run)?.eof.as_ref().map(|e| e.csv.clone())
    }

    /// Analysis > Modes.
    pub(crate) fn modes_tab(&mut self, ui: &mut Ui) {
        let runs: Vec<(String, String)> = self.modes_runs().iter().map(|m| (m.run.clone(), m.title.clone())).collect();
        let Some(current) = self.modes_current() else {
            ui.weak("Run Tools > Temporal dynamics > Temporal EOF or Lagged correlation on this series first.");
            return;
        };
        ui.horizontal_wrapped(|ui| {
            ui.label("Run");
            let width = crate::fit::combo_width(ui, runs.iter().map(|r| r.1.as_str()));
            egui::ComboBox::from_id_salt("modes run")
                .width(width.max(crate::fit::scaled(ui, 200.0)))
                .selected_text(&current.title)
                .show_ui(ui, |ui| {
                    for (run, title) in &runs {
                        if ui.selectable_label(*run == current.run, title).clicked() {
                            self.analysis.modes.run = run.clone();
                        }
                    }
                });
        });
        if let Some(eof) = current.eof.clone() {
            self.eof_modes_ui(ui, &current, &eof);
        } else if let Some(corr) = current.corr.clone() {
            self.correlation_modes_ui(ui, &current, &corr);
        }
        self.analysis.modes.drawn += 1;
    }

    fn eof_modes_ui(&mut self, ui: &mut Ui, run: &ModesRun, eof: &EofRun) {
        let Some(info) = self.session.as_ref().map(|s| s.info.clone()) else { return };
        if eof.variance.is_empty() {
            ui.weak("The run has no modes.");
            return;
        }
        let m = eof.variance.len();
        let rotated_on = self.analysis.modes.rotated && !eof.rotated.is_empty();
        ui.weak(format!(
            "{m} modes of {} dates over {} pixels. Weights: {}; solver: {}; North's n*: {}.",
            eof.dates, eof.pixels, eof.weights, eof.solver, eof.north_n
        ));
        let body = ui.text_style_height(&egui::TextStyle::Body);
        // the scree plot: the share of the variance, North's error as a bar's whiskers
        let accent = theme::accent();
        Plot::new(("modes scree", info.id))
            .height(body * 9.0)
            .x_axis_label("mode")
            .y_axis_label("% of the variance")
            .allow_drag(false)
            .allow_scroll(false)
            .allow_zoom(false)
            .allow_boxed_zoom(false)
            .include_y(0.0)
            .x_axis_formatter(|m, _| if (m.value - m.value.round()).abs() < 1e-9 { format!("{}", m.value.round()) } else { String::new() })
            .show(ui, |pu| {
                for k in 0..m {
                    let x = (k + 1) as f64;
                    let share = 100.0 * eof.variance[k];
                    let separable = eof.separable.get(k).copied().unwrap_or(true);
                    let bar = Bar::new(x, share).width(0.7).stroke(egui::Stroke::new(1.5, accent));
                    let bar = if separable { bar.fill(accent) } else { bar.fill(Color32::TRANSPARENT) };
                    pu.bar_chart(BarChart::new(format!("mode {}", k + 1), vec![bar]).color(accent));
                    if let Some(d) = eof.delta.get(k).filter(|d| d.is_finite()) {
                        let (lo, hi) = (share - 100.0 * d, share + 100.0 * d);
                        let whisker = |a: [f64; 2], b: [f64; 2]| {
                            Line::new("", PlotPoints::from(vec![a, b])).color(theme::on_plot(Color32::from_gray(200))).width(1.5)
                        };
                        pu.line(whisker([x, lo], [x, hi]));
                        pu.line(whisker([x - 0.15, lo], [x + 0.15, lo]));
                        pu.line(whisker([x - 0.15, hi], [x + 0.15, hi]));
                    }
                }
            });
        ui.label(mixed_text(&eof.separable));
        ui.weak("Filled: separable from its neighbours; hollow: mixed. The whiskers are North's error (one standard error of the share).");
        ui.separator();

        // the components
        let (pcs, label) = if rotated_on { (&eof.rotated_pcs, "R") } else { (&eof.pcs, "PC") };
        {
            let on = &mut self.analysis.modes.on;
            if on.len() != pcs.len() {
                *on = (0..pcs.len()).map(|k| k < 3).collect();
            }
        }
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Components").strong());
            if !eof.rotated.is_empty() {
                ui.checkbox(&mut self.analysis.modes.rotated, "Rotated").on_hover_text(
                    "The varimax rotation of the first modes: simpler patterns, their components no longer ordered by the variance.",
                );
            }
            for (k, on) in self.analysis.modes.on.iter_mut().enumerate() {
                let text = RichText::new(format!("{label}{}", k + 1)).color(colour(k));
                ui.checkbox(on, text);
            }
        });
        let xs: Vec<f64> = eof.years.iter().map(|&y| info.x_from_decimal_year(y)).collect();
        let weak = ui.visuals().weak_text_color();
        let current = (info.time_is_date && !info.layers.is_empty()).then(|| info.layers[self.display.t.min(info.t() - 1)].time);
        let on = self.analysis.modes.on.clone();
        let plot = Plot::new(("modes pcs", info.id))
            .height(body * 11.0)
            .y_axis_label("standard deviations")
            .legend(egui_plot::Legend::default())
            .allow_scroll(false)
            .allow_zoom(false)
            .allow_boxed_zoom(false);
        crate::chart::time_axis(plot, Some(weak)).show(ui, |pu| {
            for (k, pc) in pcs.iter().enumerate().filter(|(k, _)| on.get(*k).copied().unwrap_or(false)) {
                let z = standardized(pc);
                let pts: Vec<[f64; 2]> =
                    xs.iter().zip(&z).filter(|(_, v)| v.is_finite()).map(|(&x, &v)| [x, v]).collect();
                pu.line(Line::new(format!("{label}{}", k + 1), PlotPoints::from(pts)).color(colour(k)).width(1.8));
                // the dates left out of the fit, their components projected: markers
                let filled: Vec<[f64; 2]> = (0..xs.len())
                    .filter(|&i| eof.filled.get(i) == Some(&true) && z[i].is_finite())
                    .map(|i| [xs[i], z[i]])
                    .collect();
                if !filled.is_empty() {
                    pu.points(Points::new("", PlotPoints::from(filled)).color(colour(k)).radius(3.5));
                }
            }
            if let Some(t) = current {
                pu.vline(VLine::new("", t).color(theme::on_plot(Color32::from_rgb(255, 153, 51))).width(2.0).allow_hover(false));
            }
        });
        ui.weak("Each component in standard deviations. The orange line is the map's date.");
        ui.separator();

        // the table
        let mut show = None;
        egui::Grid::new(("modes table", info.id)).num_columns(7).striped(true).spacing([14.0, 3.0]).show(ui, |ui| {
            for h in ["Mode", "% of the variance", "Cumulative %", "North's error (%)", "Separable", "Rotated %", ""] {
                ui.label(RichText::new(h).strong());
            }
            ui.end_row();
            let mut cumulative = 0.0;
            for k in 0..m {
                cumulative += eof.variance[k];
                ui.label(format!("{}", k + 1));
                ui.label(format!("{:.1}", 100.0 * eof.variance[k]));
                ui.label(format!("{:.1}", 100.0 * cumulative));
                ui.label(eof.delta.get(k).map_or("".into(), |d| format!("{:.2}", 100.0 * d)));
                ui.label(match eof.separable.get(k) {
                    Some(true) => "yes",
                    Some(false) => "mixed",
                    None => "",
                });
                ui.label(eof.rotated.get(k).map_or("".into(), |v| format!("{:.1}", 100.0 * v)));
                let id = if rotated_on { format!("eof_rotated_{}", k + 1) } else { format!("eof_{}", k + 1) };
                let written = run.path_of(&id).is_some();
                if ui
                    .add_enabled(written, egui::Button::new("Show map"))
                    .on_hover_text("The map of this mode on top, and its pattern at the chart's pixel")
                    .on_disabled_hover_text("Only the first modes' maps were written (the Maps setting of the run)")
                    .clicked()
                {
                    show = Some(k + 1);
                }
                ui.end_row();
            }
        });
        if let Some(k) = show {
            self.modes_show_map(&run.run, k, rotated_on);
        }
        ui.horizontal_wrapped(|ui| {
            if ui.button("Copy PCs").on_hover_text("The components of every date as CSV (pcs.csv of the run)").clicked() {
                ui.ctx().copy_text(eof.csv.clone());
            }
            if ui.button("Save PCs...").clicked()
                && let Some(f) = rfd::FileDialog::new().add_filter("CSV", &["csv"]).set_file_name("pcs.csv").save_file()
                && let Err(e) = std::fs::write(&f, &eof.csv)
            {
                self.error_popup = Some(format!("{}: {e}", f.display()));
            }
        });
    }

    fn correlation_modes_ui(&mut self, ui: &mut Ui, run: &ModesRun, corr: &CorrRun) {
        let Some(info) = self.session.as_ref().map(|s| s.info.clone()) else { return };
        let unit = if corr.unit == "months" { "months" } else { "dates" };
        // the pixel: the cursor or a pin
        let mut sources: Vec<(String, String, usize, usize)> = vec![];
        if let Some(h) = &self.series.hover {
            sources.push(("cursor".into(), format!("Cursor ({}, {})", h.x, h.y), h.x, h.y));
        }
        for p in &self.series.pins {
            sources.push((format!("pin{}", p.id), format!("Pin {} ({}, {})", p.id, p.x, p.y), p.x, p.y));
        }
        // the share of significant pixels and when they are
        let map = |id: &str| run.path_of(id).and_then(|p| self.zeit.results.iter().find(|r| r.path == p));
        let n_eff = map("n_eff").map(|r| (r.path.clone(), r.x0, r.y0));
        let summary_key = (run.run.clone(), map("significant").map_or(0, |r| r.data.len()));
        if self.analysis.modes.summary.as_ref().is_none_or(|s| s.0 != summary_key)
            && let (Some(s), Some(l)) = (map("significant"), map("best_lag"))
            && s.data.len() == l.data.len()
            && !s.data.is_empty()
        {
            self.analysis.modes.summary = Some((summary_key, lag_summary(&s.data, &l.data)));
        }
        let summary = self.analysis.modes.summary.as_ref().map(|s| s.1.clone());
        if let Some(s) = &summary {
            let share = if s.valid > 0 { 100.0 * s.significant as f64 / s.valid as f64 } else { 0.0 };
            ui.label(format!(
                "{:.1} % of the pixels ({} of {}) are significant at a false discovery rate of {}.",
                share, s.significant, s.valid, corr.alpha
            ));
            if corr.p_star.is_finite() {
                ui.weak(format!(
                    "Benjamini-Hochberg: the best lag's adjusted p-value must be at most p* = {:.3e}{}.",
                    corr.p_star,
                    if corr.sidak { " (Sidak-adjusted for the lags tried)" } else { "" }
                ));
            } else {
                ui.weak("No pixel passes the false discovery rate.");
            }
        }
        let body = ui.text_style_height(&egui::TextStyle::Body);
        ui.separator();
        ui.label(RichText::new(format!("r against the lag ({unit}; the driver leads)")).strong());
        if sources.is_empty() {
            ui.weak("Hover over the map or place a pin to read r at every lag.");
        } else {
            let src = sources.iter().position(|s| s.0 == self.analysis.modes.source).unwrap_or(0);
            ui.horizontal_wrapped(|ui| {
                egui::ComboBox::from_id_salt("modes source")
                    .width(crate::fit::combo_width(ui, sources.iter().map(|s| s.1.as_str())))
                    .selected_text(&sources[src].1)
                    .show_ui(ui, |ui| {
                        for s in &sources {
                            if ui.selectable_label(s.0 == sources[src].0, &s.1).clicked() {
                                self.analysis.modes.source = s.0.clone();
                            }
                        }
                    });
            });
            let (_, _, x, y) = sources[src].clone();
            let [wx0, wy0, wx1, wy1] = run.window;
            if x < wx0 || y < wy0 || x >= wx1 || y >= wy1 {
                ui.weak("The pixel is outside the run's window.");
            } else if let Some(path) = &corr.r_lags {
                let at = (x - wx0, y - wy0);
                if self.analysis.modes.lags_at.as_ref().is_none_or(|l| l.0 != *path || (l.1, l.2) != at) {
                    let r = read_lags(path, at.0, at.1).unwrap_or_default();
                    self.analysis.modes.lags_at = Some((path.clone(), at.0, at.1, r));
                }
                let r = self.analysis.modes.lags_at.as_ref().map(|l| l.3.clone()).unwrap_or_default();
                let n_eff_here = n_eff
                    .and_then(|(p, _, _)| self.zeit.results.iter().find(|q| q.path == p).map(|q| f64::from(q.value_at(x, y))))
                    .filter(|v| v.is_finite());
                // the level a correlation must pass: the best lag's p-value adjusted for the lags tried, at p*
                let lags = corr.lags.len().max(1) as f64;
                let p_lag = if corr.sidak { 1.0 - (1.0 - corr.p_star).powf(1.0 / lags) } else { corr.p_star };
                let critical = n_eff_here.map_or(f64::NAN, |n| janus_zeit::correlation::critical_r(n, p_lag));
                let pts: Vec<[f64; 2]> =
                    corr.lags.iter().zip(&r).filter(|(_, v)| v.is_finite()).map(|(&k, &v)| [f64::from(k), v]).collect();
                let accent = theme::accent();
                Plot::new(("modes lags", info.id))
                    .height(body * 10.0)
                    .x_axis_label(format!("lag ({unit})"))
                    .y_axis_label("r")
                    .include_y(-1.0)
                    .include_y(1.0)
                    .allow_drag(false)
                    .allow_scroll(false)
                    .allow_zoom(false)
                    .allow_boxed_zoom(false)
                    .x_axis_formatter(|m, _| if (m.value - m.value.round()).abs() < 1e-9 { format!("{}", m.value.round()) } else { String::new() })
                    .show(ui, |pu| {
                        pu.hline(HLine::new("", 0.0).color(theme::on_plot(Color32::from_gray(150))).width(1.0).allow_hover(false));
                        if critical.is_finite() {
                            for sign in [1.0, -1.0] {
                                pu.hline(
                                    HLine::new("critical |r|", sign * critical)
                                        .color(theme::on_plot(theme::warning()))
                                        .style(LineStyle::dashed_dense())
                                        .width(1.5),
                                );
                            }
                        }
                        let bars: Vec<Bar> = pts
                            .iter()
                            .map(|p| Bar::new(p[0], p[1]).width(0.6).fill(accent.gamma_multiply(0.7)))
                            .collect();
                        pu.bar_chart(BarChart::new("r", bars).color(accent));
                        pu.points(Points::new("r", PlotPoints::from(pts)).color(accent).radius(3.5));
                    });
                let best = corr
                    .lags
                    .iter()
                    .zip(&r)
                    .filter(|(_, v)| v.is_finite())
                    .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()));
                if let Some((k, v)) = best {
                    ui.label(format!("Strongest: lag {k} {unit}, r = {v:.3}."));
                }
                if critical.is_finite() {
                    ui.weak(format!(
                        "Dashed: |r| = {critical:.3}, what this pixel's {:.1} effective pairs need at p = {p_lag:.2e} (the false discovery rate's threshold for one lag).",
                        n_eff_here.unwrap_or(f64::NAN)
                    ));
                } else {
                    ui.weak("No critical level here: the pixel has too few effective pairs, or no pixel passed the false discovery rate.");
                }
            } else {
                ui.weak("The run did not write r at every lag.");
            }
        }
        // when the significant pixels' correlations are best
        if let Some(s) = summary.filter(|s| s.significant > 0) {
            ui.separator();
            ui.label(RichText::new(format!("Best lag of the significant pixels ({unit})")).strong());
            let accent = theme::accent();
            let bars: Vec<Bar> =
                s.hist.iter().map(|&(k, n)| Bar::new(f64::from(k), n as f64).width(0.8).fill(accent)).collect();
            Plot::new(("modes best lag", info.id))
                .height(body * 8.0)
                .x_axis_label(format!("lag ({unit})"))
                .y_axis_label("pixels")
                .allow_drag(false)
                .allow_scroll(false)
                .allow_zoom(false)
                .allow_boxed_zoom(false)
                .x_axis_formatter(|m, _| if (m.value - m.value.round()).abs() < 1e-9 { format!("{}", m.value.round()) } else { String::new() })
                .show(ui, |pu| pu.bar_chart(BarChart::new("pixels", bars).color(accent)));
        }
    }

    /// The mode on top of the maps at a pixel of the active series: its name ("EOF 1") and the pixel's seasonal
    /// cycle with the mode's pattern times its component put on it, `[time, value]` at every date it is defined (the
    /// series' units). None when no mode's map is visible, or the pixel has no loading.
    pub(crate) fn eof_overlay(&self, at: (usize, usize), values: &[f32]) -> Option<(String, Vec<[f64; 2]>)> {
        let info = &self.session.as_ref()?.info;
        if !info.time_is_date || values.len() != info.t() {
            return None;
        }
        let r = self.zeit.results.iter().rev().find(|r| {
            r.cube == info.id && r.visible && r.eof.as_ref().is_some_and(|m| m.kind != ModeKind::Corr)
        })?;
        let mode = r.eof.as_ref().filter(|m| m.pc.len() == values.len())?;
        let loading = f64::from(r.value_at(at.0, at.1));
        if !loading.is_finite() {
            return None;
        }
        let days: Vec<i64> = (0..info.t()).map(|t| info.unix_day(t)).collect();
        let v: Vec<f64> = values.iter().map(|&x| f64::from(x)).collect();
        let settings = janus_zeit::eof::anomaly_settings(&mode.params);
        let overlay = janus_zeit::eof::pixel_overlay(&v, &days, &settings, loading, &mode.pc);
        let pts = (0..info.t()).filter(|&t| overlay[t].is_finite()).map(|t| [info.layers[t].time, overlay[t]]).collect();
        Some((mode.label(), pts))
    }

    /// The overlay drawn on the chart: dashed, in the series' colour, as the chart shows the series ("EOF 1 at the
    /// cursor").
    pub(crate) fn draw_eof_overlay(
        &self,
        plot: &mut PlotUi,
        shown: &crate::chart::AsShown,
        at: (usize, usize),
        values: &[f32],
        place: &str,
        color: Color32,
    ) {
        let Some((name, pts)) = self.eof_overlay(at, values) else { return };
        let pts: Vec<[f64; 2]> =
            pts.into_iter().map(|[x, y]| [x, shown.at(y, x)]).filter(|p| p[1].is_finite()).collect();
        plot.line(
            Line::new(format!("{name} {place}"), PlotPoints::from(pts))
                .color(color)
                .width(2.5)
                .style(LineStyle::dashed_dense()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mixed_modes_are_named_in_runs() {
        assert!(mixed_text(&[true, true, true]).starts_with("Every mode is separable"));
        let t = mixed_text(&[true, true, false, false, true]);
        assert!(t.starts_with("Modes 3 and 4 are mixed (North et al. 1982): read them together or rotate"), "{t}");
        let t = mixed_text(&[false, false, false, true, false]);
        assert!(t.starts_with("Modes 1, 2 and 3 are mixed; mode 5 is mixed with its neighbour"), "{t}");
    }

    #[test]
    fn a_component_is_standardized() {
        let z = standardized(&[1.0, 2.0, 3.0, f64::NAN, 4.0, 5.0]);
        let v: Vec<f64> = z.iter().copied().filter(|x| x.is_finite()).collect();
        assert_eq!(v.len(), 5);
        assert!(v.iter().sum::<f64>().abs() < 1e-12);
        assert!(((v.iter().map(|x| x * x).sum::<f64>() / 4.0) - 1.0).abs() < 1e-12);
        assert!(z[3].is_nan());
    }

    #[test]
    fn the_best_lags_are_counted_over_the_significant_pixels() {
        let s = [2.0, 1.0, 2.0, f32::NAN, 2.0, 1.0];
        let l = [2.0, 3.0, 2.0, 1.0, 0.0, 2.0];
        let r = lag_summary(&s, &l);
        assert_eq!((r.valid, r.significant), (5, 3));
        assert_eq!(r.hist, vec![(0, 1), (2, 2)]);
    }
}

impl State {
    /// The EOF runs of result maps read from a project: found again from their folders (eof.json, pcs.csv, job.json)
    /// and their maps given their modes, so Analysis > Modes and the chart's overlay work as after the run.
    pub(crate) fn restore_modes(&mut self) {
        let runs: Vec<String> = self
            .zeit
            .results
            .iter()
            .filter(|r| r.eof.is_none() && !r.run.is_empty() && !r.preview)
            .map(|r| r.run.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .filter(|r| !self.zeit.modes_checked.contains(r))
            .collect();
        for run in runs {
            self.zeit.modes_checked.insert(run.clone());
            if !Path::new(&run).join("eof.json").is_file() {
                continue;
            }
            let Some(first) = self.zeit.results.iter().find(|r| r.run == run) else { continue };
            let (cube, window) = (first.cube, [first.x0, first.y0, first.x0 + first.w, first.y0 + first.h]);
            let params = run_params("eof", &run);
            let outputs: Vec<Value> = self
                .zeit
                .results
                .iter()
                .filter(|r| r.run == run)
                .filter_map(|r| {
                    let stem = Path::new(&r.path).file_stem()?.to_str()?;
                    Some(serde_json::json!({ "id": stem.strip_prefix("eof_").unwrap_or(stem), "path": r.path }))
                })
                .collect();
            let Some(m) = ModesRun::new("eof", &run, cube, window, "Temporal EOF", &outputs, &params) else { continue };
            if !self.zeit.modes_runs.iter().any(|r| r.run == run)
                && let Some(e) = m.eof.clone()
            {
                for r in self.zeit.results.iter_mut().filter(|r| r.run == run) {
                    let id = m.maps.iter().find(|(_, p)| *p == r.path).map(|(i, _)| i.clone()).unwrap_or_default();
                    let (kind, k) = if let Some(k) = id.strip_prefix("eof_rotated_") {
                        (ModeKind::Rotated, k)
                    } else if let Some(k) = id.strip_prefix("eof_") {
                        (ModeKind::Eof, k)
                    } else if let Some(k) = id.strip_prefix("corrmap_") {
                        (ModeKind::Corr, k)
                    } else {
                        continue;
                    };
                    let Ok(k) = k.parse::<usize>() else { continue };
                    let (pcs, shares) = if kind == ModeKind::Rotated { (&e.rotated_pcs, &e.rotated) } else { (&e.pcs, &e.variance) };
                    let Some(pc) = pcs.get(k.wrapping_sub(1)) else { continue };
                    r.eof = Some(EofMode {
                        mode: k,
                        kind,
                        variance: shares.get(k - 1).copied().unwrap_or(f64::NAN),
                        north_error: (kind == ModeKind::Eof).then(|| e.delta.get(k - 1).copied()).flatten(),
                        separable: (kind == ModeKind::Eof).then(|| e.separable.get(k - 1).copied()).flatten(),
                        pc: pc.clone(),
                        params: params.clone(),
                    });
                }
                self.zeit.modes_runs.push(m);
            }
        }
    }
}
