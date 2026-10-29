//! Where each result came from (phase 52): a run's provenance.json beside its job.json and its log. The tool, Janus'
//! and Zeit's versions, when it ran and for how long, the series it read (its files, the band or expression shown,
//! the quality mask), the scope and the window, the parameters (those changed from the tool's defaults named), the
//! files it wrote; for a run of a Zeit tool, its Python (rerun.py: the same Zeit calls over the same window). From
//! it, in a result's menu: the Provenance window, Run again with these parameters, Copy Python of the run, the
//! folder.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::app::State;
use crate::theme;
use crate::zeit::JobState;

pub const FILE: &str = "provenance.json";
/// a Zeit run's Python, beside its provenance
pub const SCRIPT: &str = "rerun.py";

/// The series a run read.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SeriesRecord {
    pub name: String,
    pub description: String,
    /// the band, the normalized difference or the expression shown, and the quality mask
    pub selection: String,
    pub expression: String,
    pub qa_band: i32,
    pub qa_rule: String,
    pub dates: usize,
    pub first: String,
    pub last: String,
    pub width: usize,
    pub height: usize,
    pub crs: String,
    /// what the layer opened (files, a folder, a URL), and each date's file
    pub inputs: Vec<String>,
    pub files: Vec<String>,
}

impl SeriesRecord {
    pub fn of(info: &janus_cube::CubeInfo, name: &str, inputs: &[String]) -> SeriesRecord {
        let n = info.t();
        let label = |t: usize| info.layers.get(t).map_or(String::new(), |l| l.label.clone());
        SeriesRecord {
            name: name.to_string(),
            description: info.description.clone(),
            selection: info.selection_text(),
            expression: info.expr.as_ref().map_or(String::new(), |e| e.text.clone()),
            qa_band: info.sel.qa_band,
            qa_rule: info.sel.qa_rule.id().to_string(),
            dates: n,
            first: label(0),
            last: label(n.saturating_sub(1)),
            width: info.width,
            height: info.height,
            crs: if info.crs_authority.is_empty() { info.crs_name.clone() } else { info.crs_authority.clone() },
            inputs: inputs.iter().map(|p| absolute(p)).collect(),
            files: info
                .layers
                .iter()
                .map(|l| absolute(&l.path))
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect(),
        }
    }

    /// The same series as `info` (its files and what it shows), wherever it was opened from.
    pub fn matches(&self, info: &janus_cube::CubeInfo) -> bool {
        let files: std::collections::BTreeSet<String> = info.layers.iter().map(|l| absolute(&l.path)).collect();
        files.len() == self.files.len()
            && self.files.iter().all(|f| files.contains(&absolute(f)))
            && info.selection_text() == self.selection
    }
}

/// A file or folder on disk as an absolute path (a run's record and its script work from anywhere); a URL or
/// another of GDAL's paths as it is.
pub(crate) fn absolute(p: &str) -> String {
    match std::path::Path::new(p).exists().then(|| std::path::absolute(p)) {
        Some(Ok(a)) => a.display().to_string(),
        _ => p.to_string(),
    }
}

/// A file a run wrote.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct OutputRecord {
    pub name: String,
    pub path: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Provenance {
    pub format: u32,
    /// "zeit" (a tool of the Tools menu), "model" (a deep learning model applied), "calculator"
    pub kind: String,
    pub tool: String,
    pub tool_name: String,
    pub janus: String,
    pub zeit: String,
    pub zeit_core: String,
    /// "YYYY-MM-DD HH:MM:SS UTC"
    pub started: String,
    pub seconds: f64,
    /// running, done, failed, cancelled
    pub state: String,
    pub error: String,
    pub series: Option<SeriesRecord>,
    /// the scope as the tool's window names it, its place there, the window in the series' pixels
    pub scope: String,
    pub scope_index: usize,
    pub window: Option<[usize; 4]>,
    pub threads: Option<i64>,
    pub params: BTreeMap<String, Value>,
    /// the parameters not at the tool's default
    pub changed: Vec<String>,
    /// what else the run took: a model's file and its post-processing, the calculator's text, layers and options
    pub extra: BTreeMap<String, Value>,
    pub outputs: Vec<OutputRecord>,
    /// the run's Python (beside it), or why it has none
    pub python: String,
    pub python_note: String,
}

impl Provenance {
    /// A run starting now.
    pub fn start(kind: &str, tool: &str, tool_name: &str) -> Provenance {
        Provenance {
            format: 1,
            kind: kind.into(),
            tool: tool.into(),
            tool_name: tool_name.into(),
            janus: env!("CARGO_PKG_VERSION").into(),
            zeit: janus_zeit::ZEIT_VERSION.into(),
            zeit_core: janus_zeit::ZEIT_CORE_REV.into(),
            started: crate::zeit::timestamp_text(),
            state: "running".into(),
            ..Default::default()
        }
    }

    /// The parameters given, and those not at the tool's default (a Zeit tool's).
    pub fn set_params(&mut self, params: &BTreeMap<String, Value>) {
        self.params = params.clone();
        self.changed = janus_zeit::manifest().tool(&self.tool).map_or(vec![], |t| {
            t.params
                .iter()
                .filter(|p| params.get(&p.id).is_some_and(|v| *v != p.default))
                .map(|p| p.id.clone())
                .collect()
        });
    }

    pub fn read(dir: &Path) -> Result<Provenance, String> {
        let path = dir.join(FILE);
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn write(&self, dir: &Path) {
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(dir.join(FILE), text);
        }
    }
}

/// The folder of the provenance of a result's file or folder (a run's folder, or the one above: a series a run
/// wrote in a folder of its own, the calculator's VRTs under its GeoTIFFs').
pub fn find(path: &str) -> Option<PathBuf> {
    let p = Path::new(path);
    let mut dirs = vec![];
    if p.is_dir() {
        dirs.push(p.to_path_buf());
    }
    let mut up = p.parent();
    for _ in 0..2 {
        let Some(d) = up else { break };
        dirs.push(d.to_path_buf());
        up = d.parent();
    }
    dirs.into_iter().find(|d| d.join(FILE).is_file())
}

/// The files a run left in its folder (its maps, VRTs, vector files), when it did not list them.
fn files_in(dir: &Path) -> Vec<OutputRecord> {
    let mut v: Vec<OutputRecord> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let ext = p.extension().map_or(String::new(), |e| e.to_string_lossy().to_lowercase());
            matches!(ext.as_str(), "tif" | "tiff" | "vrt" | "gpkg" | "nc" | "csv")
        })
        .map(|p| OutputRecord {
            name: p.file_stem().map_or(String::new(), |s| s.to_string_lossy().into_owned()),
            path: p.to_string_lossy().into_owned(),
        })
        .collect();
    v.sort_by(|a, b| a.path.cmp(&b.path));
    v
}

/// The Provenance window's state, and the runs whose provenance is written once they end.
#[derive(Default)]
pub struct ProvenanceUi {
    /// the window: the run's folder and its provenance
    pub shown: Option<(PathBuf, Provenance)>,
    /// runs under way: their folder and provenance so far
    pending: Vec<(String, Provenance)>,
    /// the run's Python was copied (the window says so for a moment)
    copied: Option<std::time::Instant>,
}

impl State {
    /// A run started in `dir`: its provenance so far written (a run that never ends leaves it "running").
    pub(crate) fn begin_provenance(&mut self, dir: &Path, p: Provenance) {
        p.write(dir);
        self.prov.pending.push((dir.to_string_lossy().into_owned(), p));
    }

    /// Runs that ended: how long, how, the files they wrote, added to their provenance. Before the finished runs'
    /// maps are taken (their outputs are listed in the job's result).
    pub(crate) fn pump_provenance(&mut self) {
        let mut k = 0;
        while k < self.prov.pending.len() {
            let dir = self.prov.pending[k].0.clone();
            let job = self.zeit.jobs.iter().find(|j| j.output_dir == dir);
            let Some(job) = job else {
                // removed from the Tasks before it ended: as it is
                self.prov.pending.remove(k);
                continue;
            };
            let s = job.status.lock().unwrap();
            if s.state == JobState::Running {
                k += 1;
                continue;
            }
            let (mut p, dir) = (self.prov.pending.remove(k).1, PathBuf::from(dir));
            p.seconds = s.seconds;
            p.state = match s.state {
                JobState::Done => "done",
                JobState::Failed => "failed",
                JobState::Cancelled => "cancelled",
                JobState::Running => "running",
            }
            .into();
            p.error = s.error.clone();
            p.outputs = match &s.result {
                Some(r) => r
                    .outputs
                    .iter()
                    .map(|o| OutputRecord {
                        name: o["name"].as_str().or(o["id"].as_str()).unwrap_or("").to_string(),
                        path: o["path"].as_str().unwrap_or("").to_string(),
                    })
                    .collect(),
                None if s.state == JobState::Done => files_in(&dir),
                None => vec![],
            };
            drop(s);
            p.write(&dir);
        }
    }

    /// The Provenance window on the run of `dir` (a run's folder, or a file in it).
    pub(crate) fn show_provenance(&mut self, path: &str) {
        let Some(dir) = find(path) else {
            self.error_popup = Some(format!("{path}: no {FILE} beside it (made by a Janus before 1.6)"));
            return;
        };
        match Provenance::read(&dir) {
            Ok(p) => self.prov.shown = Some((dir, p)),
            Err(e) => self.error_popup = Some(e),
        }
    }

    /// The run's Python (a Zeit tool's), copied to the clipboard; its text, or why there is none.
    pub(crate) fn copy_run_python(&mut self, ctx: &egui::Context, path: &str) -> Result<String, String> {
        let dir = find(path).ok_or("no provenance")?;
        let p = Provenance::read(&dir)?;
        if p.python.is_empty() {
            return Err(if p.python_note.is_empty() { "this run has no Python".into() } else { p.python_note });
        }
        let code = std::fs::read_to_string(dir.join(&p.python)).map_err(|e| e.to_string())?;
        ctx.copy_text(code.clone());
        self.prov.copied = Some(std::time::Instant::now());
        Ok(code)
    }

    /// The window of the run's tool with its parameters, scope and window (on the same series: the run's window
    /// itself), ready to run again.
    pub(crate) fn run_again(&mut self, p: &Provenance) {
        match p.kind.as_str() {
            "zeit" => {
                let Some(tool) = janus_zeit::manifest().tool(&p.tool) else {
                    self.error_popup = Some(format!("{}: not a tool of this Janus", p.tool));
                    return;
                };
                *self.zeit.params(tool) = p.params.clone();
                let info = self.session.as_ref().map(|s| s.info.clone());
                let same = matches!((&p.series, &info), (Some(s), Some(i)) if s.matches(i));
                let roi = self.series.roi_rect.is_some();
                let u = self.zeit.tools.entry(tool.id.clone()).or_default();
                u.version += 1;
                u.again = None;
                u.again_note.clear();
                match (same, p.window, &info) {
                    (true, Some(w), Some(i)) if p.scope_index != 0 => {
                        // the very window (an ROI since moved, the area then visible)
                        u.again = Some((i.id, w));
                        u.scope = 3;
                    }
                    _ => {
                        u.scope = if p.scope_index == 2 && !roi { 0 } else { p.scope_index.min(2) };
                    }
                }
                if !same && let Some(s) = &p.series {
                    u.again_note = format!("The run was on {}; this runs on the active layer.", s.name);
                }
                self.set_tool_open(tool, true);
            }
            "model" => {
                let model = p.extra.get("model").and_then(Value::as_str).unwrap_or("").to_string();
                match janus_ml::Model::read_header(&model) {
                    Ok(h) => self.ml.model = Some((model, h)),
                    Err(e) => {
                        self.error_popup = Some(e);
                        return;
                    }
                }
                let f = |k: &str| p.extra.get(k).and_then(Value::as_f64);
                let post = &mut self.ml.post;
                post.smooth = p.extra.get("smooth").and_then(Value::as_bool).unwrap_or(false);
                post.window = f("smooth_window").map_or(post.window, |v| v as usize);
                post.smoothness = f("smoothness").unwrap_or(post.smoothness);
                post.neigh_fraction = f("neigh_fraction").unwrap_or(post.neigh_fraction);
                post.min_size = f("min_size").map_or(0, |v| v as usize);
                self.ml.aoa = p.extra.get("aoa").and_then(Value::as_bool).unwrap_or(false);
                self.ml.scope = if p.scope_index == 2 && self.series.roi_rect.is_none() { 0 } else { p.scope_index };
                self.ml.open = true;
            }
            "calculator" => {
                let s = |k: &str| p.extra.get(k).and_then(Value::as_str).map(str::to_string);
                let c = &mut self.calc;
                c.text = s("text").unwrap_or_default();
                c.out_name = s("name").unwrap_or_default();
                c.name_typed = !c.out_name.is_empty();
                c.cog = p.extra.get("cog").and_then(Value::as_bool).unwrap_or(false);
                c.apply_qa = p.extra.get("apply_qa").and_then(Value::as_bool).unwrap_or(c.apply_qa);
                c.show = true;
            }
            k => self.error_popup = Some(format!("a run of kind \"{k}\" cannot be run again")),
        }
    }

    /// The Provenance window: what the run took and did; Run again, Copy Python, the folder.
    pub(crate) fn provenance_window(&mut self, ctx: &egui::Context) {
        let Some((dir, p)) = self.prov.shown.clone() else { return };
        let mut open = true;
        let (mut again, mut copy) = (false, false);
        let copied = self.prov.copied.is_some_and(|t| t.elapsed().as_secs_f64() < 2.0);
        egui::Window::new("Provenance").open(&mut open).default_width(560.0).show(ctx, |ui| {
            let title = if p.tool_name.is_empty() { &p.tool } else { &p.tool_name };
            ui.heading(title);
            let state = match p.state.as_str() {
                "done" => format!("done in {}", duration(p.seconds)),
                "running" => "running (or ended with Janus closed)".into(),
                s => format!("{s} after {}", duration(p.seconds)),
            };
            ui.label(format!("{}, {state}", p.started));
            if !p.error.is_empty() {
                ui.colored_label(theme::error(), &p.error);
            }
            ui.weak(format!(
                "Janus {}, Zeit {} (zeit-core {})",
                p.janus,
                p.zeit,
                &p.zeit_core[..p.zeit_core.len().min(10)]
            ));
            egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                egui::Grid::new("provenance").num_columns(2).spacing([12.0, 4.0]).striped(true).show(ui, |ui| {
                    if let Some(s) = &p.series {
                        ui.label("Series");
                        ui.label(format!("{} ({} dates, {} .. {})", s.name, s.dates, s.first, s.last));
                        ui.end_row();
                        ui.label("");
                        ui.weak(format!("{}, {} x {} px, {}", s.description, s.width, s.height, s.crs));
                        ui.end_row();
                        if !s.selection.is_empty() {
                            ui.label("Shown");
                            ui.label(&s.selection);
                            ui.end_row();
                        }
                        ui.label("Files");
                        ui.vertical(|ui| {
                            for f in s.files.iter().take(6) {
                                ui.weak(f);
                            }
                            if s.files.len() > 6 {
                                ui.weak(format!("... {} files in all", s.files.len()));
                            }
                        });
                        ui.end_row();
                    }
                    if !p.scope.is_empty() {
                        ui.label("Scope");
                        ui.label(match p.window {
                            Some([x0, y0, x1, y1]) => {
                                format!(
                                    "{}: columns {x0}..{x1}, rows {y0}..{y1} ({} x {} px)",
                                    p.scope,
                                    x1 - x0,
                                    y1 - y0
                                )
                            }
                            None => p.scope.clone(),
                        });
                        ui.end_row();
                    }
                    if let Some(n) = p.threads.filter(|&n| n > 0) {
                        ui.label("Threads");
                        ui.label(n.to_string());
                        ui.end_row();
                    }
                    let tool = janus_zeit::manifest().tool(&p.tool);
                    for (k, v) in &p.params {
                        let changed = p.changed.contains(k);
                        let name = tool
                            .and_then(|t| t.params.iter().find(|q| &q.id == k))
                            .map_or(k.clone(), |q| if q.label.is_empty() { k.clone() } else { q.label.clone() });
                        let mut text = value_text(v);
                        if changed && let Some(d) = tool.and_then(|t| t.params.iter().find(|q| &q.id == k)) {
                            text += &format!("   (default {})", value_text(&d.default));
                        }
                        if changed {
                            ui.label(egui::RichText::new(name).color(theme::accent()).strong());
                            ui.label(egui::RichText::new(text).color(theme::accent()));
                        } else {
                            ui.label(name);
                            ui.weak(text);
                        }
                        ui.end_row();
                    }
                    for (k, v) in &p.extra {
                        ui.label(k.replace('_', " "));
                        ui.add(egui::Label::new(value_text(v)).wrap());
                        ui.end_row();
                    }
                    if !p.outputs.is_empty() {
                        ui.label("Outputs");
                        ui.vertical(|ui| {
                            for o in &p.outputs {
                                ui.label(&o.name).on_hover_text(&o.path);
                            }
                        });
                        ui.end_row();
                    }
                });
            });
            if !p.changed.is_empty() {
                ui.weak("In the accent colour: the parameters not at the tool's default.");
            }
            ui.separator();
            ui.horizontal(|ui| {
                if ui
                    .button("Run again with these parameters...")
                    .on_hover_text(
                        "The tool's window with them and the same scope (the same window on the same series)",
                    )
                    .clicked()
                {
                    again = true;
                }
                let py = ui.add_enabled(!p.python.is_empty(), egui::Button::new("Copy Python"));
                let py = if p.python.is_empty() {
                    py.on_disabled_hover_text(&p.python_note)
                } else {
                    py.on_hover_text(
                        "A script that reads the series over the run's window from its files, makes the same Zeit \
                         calls and writes their maps as GeoTIFFs",
                    )
                };
                copy = py.clicked();
                if ui.button("Open folder").clicked() {
                    crate::platform::open_in_explorer(&dir.to_string_lossy());
                }
                if copied {
                    ui.weak("copied");
                }
            });
        });
        if again {
            self.run_again(&p);
        }
        if copy && let Err(e) = self.copy_run_python(ctx, &dir.to_string_lossy()) {
            self.error_popup = Some(e);
        }
        if !open {
            self.prov.shown = None;
        }
    }
}

fn duration(s: f64) -> String {
    if s < 60.0 { format!("{s:.1} s") } else { format!("{:.0} min {:.0} s", (s / 60.0).floor(), s % 60.0) }
}

/// A parameter's value as text: a list or table cut short.
fn value_text(v: &Value) -> String {
    let t = match v {
        Value::String(s) => s.clone(),
        v => v.to_string(),
    };
    if t.chars().count() > 160 { format!("{}...", t.chars().take(160).collect::<String>()) } else { t }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_parameters_and_round_trip() {
        let mut p = Provenance::start("zeit", "mann_kendall", "Mann-Kendall trend");
        let mut params: BTreeMap<String, Value> = janus_zeit::manifest()
            .tool("mann_kendall")
            .unwrap()
            .params
            .iter()
            .map(|q| (q.id.clone(), q.default.clone()))
            .collect();
        params.insert("alpha".into(), 0.1.into());
        p.set_params(&params);
        assert_eq!(p.changed, vec!["alpha".to_string()]);
        let dir = std::env::temp_dir().join(format!("janus-prov-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        p.write(&dir);
        assert_eq!(Provenance::read(&dir).unwrap(), p);
        // found from a file of the run, and from one a level down
        let f = dir.join("sub").join("x.tif");
        std::fs::write(&f, b"").unwrap();
        assert_eq!(find(&f.to_string_lossy()), Some(dir.clone()));
        assert_eq!(find(&dir.to_string_lossy()), Some(dir.clone()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
