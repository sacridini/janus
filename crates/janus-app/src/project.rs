//! Projects (phase 54): File > New project, Open project..., Save project (Ctrl+S), Save project as..., Recent
//! projects. A `.janus` file is JSON with a format number: each series in the drawing order (its files, relative to
//! the project's folder when they can be, the absolute path kept as a fallback; its band selection or expression,
//! name, visibility and style: mode, colormaps, ranges, stretch, RGB bands, opacity, blend mode, classes; its date),
//! the vector layers (file, layer, name, colour), the runs' maps (file, colours, the series they lie on, their run's
//! folder and its provenance), the drawing order and the groups, the pins, the ROI and the transect in the active
//! layer's pixels, the view (its centre and width, as a saved view), the swipe or lens, the map panels, the basemap,
//! the saved views, the season and the active layer.
//!
//! Opening one closes what is open and opens its layers one at a time by the usual way of opening, each layer's
//! state applied as it opens; the rest once the last is open. A file gone is a notice, not an error that stops the
//! rest. The session is kept at exit (and every minute) in the settings' folder and opened again when Janus starts
//! without files (Settings, on by default). The project's name, with a `*` while it has changes not saved, is in the
//! window's title.

use std::path::{Component, Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::app::State;
use crate::contents::Sel;
use crate::order::Item;

pub const EXTENSION: &str = "janus";
/// a project's format: a file of a later one is read as far as it can be
pub const FORMAT: u32 = 1;

/// A file of a layer: relative to the project's folder (when on the same drive), and absolute.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct FileRef {
    pub path: String,
    pub absolute: String,
}

impl FileRef {
    pub fn new(path: &str, base: Option<&Path>) -> FileRef {
        // a path given relative to the working folder (the command line's): made absolute
        let path = if is_virtual(path) || path.is_empty() {
            path.to_string()
        } else {
            std::path::absolute(path)
                .map_or_else(|_| path.to_string(), |p| normalize(&p).to_string_lossy().into_owned())
        };
        let rel = base.and_then(|b| relative(Path::new(&path), b)).unwrap_or_else(|| path.clone());
        FileRef { path: rel, absolute: path }
    }

    /// Where it is now: relative to the project's folder, else its absolute path, else None (gone). URLs and
    /// GDAL's own names (NETCDF:..., /vsis3/...) as they are.
    pub fn resolve(&self, base: Option<&Path>) -> Option<String> {
        if is_virtual(&self.absolute) || is_virtual(&self.path) {
            return Some(if self.absolute.is_empty() { self.path.clone() } else { self.absolute.clone() });
        }
        if let Some(b) = base {
            let p = b.join(&self.path);
            if !self.path.is_empty() && p.exists() {
                return Some(normalize(&p).to_string_lossy().into_owned());
            }
        }
        for p in [&self.absolute, &self.path] {
            if !p.is_empty() && Path::new(p).is_absolute() && Path::new(p).exists() {
                return Some(p.clone());
            }
        }
        None
    }

    pub fn shown(&self) -> &str {
        if self.absolute.is_empty() { &self.path } else { &self.absolute }
    }
}

/// A URL, or a name GDAL reads that is not a file of the disk.
fn is_virtual(p: &str) -> bool {
    p.contains("://") || p.starts_with("/vsi") || p.contains(':') && !Path::new(p).is_absolute() && p.len() > 2
}

/// `.` and `..` taken out of a path.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

/// `path` relative to the folder `base` (with `..` as needed), when both are on the same drive.
pub fn relative(path: &Path, base: &Path) -> Option<String> {
    if !path.is_absolute() || !base.is_absolute() {
        return None;
    }
    let (p, b) = (normalize(path), normalize(base));
    let pc: Vec<Component> = p.components().collect();
    let bc: Vec<Component> = b.components().collect();
    let same = |a: &Component, b: &Component| {
        a.as_os_str().to_string_lossy().eq_ignore_ascii_case(&b.as_os_str().to_string_lossy())
    };
    // the same root (drive)
    if pc.first().zip(bc.first()).is_none_or(|(a, b)| !same(a, b)) {
        return None;
    }
    let common = pc.iter().zip(&bc).take_while(|(a, b)| same(a, b)).count();
    let mut parts: Vec<String> = std::iter::repeat_n("..".to_string(), bc.len() - common).collect();
    parts.extend(pc[common..].iter().map(|c| c.as_os_str().to_string_lossy().into_owned()));
    Some(if parts.is_empty() { ".".into() } else { parts.join("/") })
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Selection {
    pub band: i32,
    pub nd_band: i32,
    pub qa_band: i32,
    pub qa_rule: String,
    pub chosen: bool,
    pub expr_name: String,
    pub expr_text: String,
    pub stack: Option<bool>,
}

impl Selection {
    fn of(s: &janus_cube::BandSelection) -> Selection {
        Selection {
            band: s.band,
            nd_band: s.nd_band,
            qa_band: s.qa_band,
            qa_rule: s.qa_rule.id().into(),
            chosen: s.chosen,
            expr_name: if s.expr.is_some() { s.expr.name() } else { String::new() },
            expr_text: if s.expr.is_some() { s.expr.text() } else { String::new() },
            stack: s.stack,
        }
    }

    pub(crate) fn band_selection(&self) -> janus_cube::BandSelection {
        use janus_cube::QaRule;
        let rule = [QaRule::Fmask, QaRule::LandsatC2, QaRule::NonZero, QaRule::Scl]
            .into_iter()
            .find(|r| r.id() == self.qa_rule)
            .unwrap_or(QaRule::None);
        janus_cube::BandSelection {
            band: self.band.max(1),
            nd_band: self.nd_band,
            qa_band: self.qa_band,
            qa_rule: rule,
            chosen: self.chosen,
            expr: if self.expr_text.is_empty() {
                janus_cube::calc::ExprId::NONE
            } else {
                janus_cube::calc::ExprId::intern(&self.expr_name, &self.expr_text)
            },
            stack: self.stack,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SeriesEntry {
    pub name: String,
    pub inputs: Vec<FileRef>,
    pub selection: Selection,
    pub visible: bool,
    /// style.rs's text (mode, colormaps, ranges, stretch, RGB bands, opacity, blend, classes)
    pub style: String,
    /// the date shown: its label, and its place
    pub date: String,
    pub t: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct VectorEntry {
    pub file: FileRef,
    pub layer: String,
    pub name: String,
    pub visible: bool,
    pub color: [u8; 4],
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ResultEntry {
    /// the series it lies on (its place in `series`)
    pub series: usize,
    pub file: FileRef,
    /// its run's folder (its provenance) and its place among the run's outputs
    pub run: FileRef,
    pub output: usize,
    pub name: String,
    pub source: String,
    pub unit: String,
    pub colormap: String,
    pub lo: f32,
    pub hi: f32,
    pub visible: bool,
    pub opacity: f32,
    pub classes: Vec<String>,
    /// the window in its series' pixels
    pub window: [usize; 4],
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct GroupEntry {
    pub name: String,
    /// "s0", "v1", "r2": a series, a vector layer, a run's map by its place in the project's lists
    pub members: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SwipeEntry {
    /// the layer compared (None: the basemap alone)
    pub series: Option<usize>,
    pub x: f32,
    pub lens: bool,
    pub radius: f32,
    pub own_date: bool,
    pub t: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MapViewEntry {
    pub series: usize,
    pub own_date: bool,
    pub t: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct BasemapEntry {
    pub source: String,
    pub on: bool,
    pub opacity: f32,
    pub url: String,
    pub max_zoom: i32,
    pub tile_size: i32,
    pub attribution: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Project {
    pub format: u32,
    pub janus: String,
    pub series: Vec<SeriesEntry>,
    pub vectors: Vec<VectorEntry>,
    pub results: Vec<ResultEntry>,
    /// the drawing order, bottom to top ("s0", "v1", "r2")
    pub order: Vec<String>,
    pub groups: Vec<GroupEntry>,
    /// the active series (its place in `series`)
    pub active: Option<usize>,
    /// in the active layer's pixels
    pub pins: Vec<[usize; 2]>,
    /// each pin's class ("" none) for the classifiers, and the classes in their order
    pub pin_classes: Vec<String>,
    pub pin_class_list: Vec<String>,
    pub roi: Option<[usize; 4]>,
    pub roi_polygon: Vec<Vec<f64>>,
    pub transect: Vec<[f64; 2]>,
    /// the view (navigation.rs's saved view text)
    pub view: String,
    pub swipe: Option<SwipeEntry>,
    pub map_views: Vec<MapViewEntry>,
    pub basemap: BasemapEntry,
    pub saved_views: Vec<String>,
    /// the season: on, first and last day of the year
    pub season: (bool, i32, i32),
    /// the last session's file only: the project it was, and whether it had changes not saved
    pub project_file: String,
    pub unsaved: bool,
}

impl Project {
    pub fn read(path: &Path) -> Result<Project, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let p: Project =
            serde_json::from_str(&text).map_err(|e| format!("{}: not a Janus project ({e})", path.display()))?;
        if p.format == 0 {
            return Err(format!("{}: not a Janus project", path.display()));
        }
        Ok(p)
    }

    pub fn write(&self, path: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        // written beside, then moved over: a project is never left half written
        let tmp = path.with_extension("janus.tmp");
        std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// What tells whether a project changed: everything but the view (panning and zooming are not changes).
    fn fingerprint(&self) -> String {
        let mut p = self.clone();
        (p.view, p.project_file, p.unsaved) = (String::new(), String::new(), false);
        serde_json::to_string(&p).unwrap_or_default()
    }
}

/// A style as a project keeps it: the ranges not set by hand left out (computed again from the values).
pub(crate) fn kept_style(mut st: crate::style::LayerStyle) -> crate::style::LayerStyle {
    if let crate::style::LayerStyle::Series(s) = &mut st {
        for r in s.ranges.iter_mut().filter(|r| !r.2) {
            *r = (0.0, 0.0, false);
        }
    }
    st
}

/// A run's map as a project keeps it, to be read (zeit.rs's load_result_layer) from `file` onto series `cube`.
pub(crate) fn result_layer(
    e: &ResultEntry,
    file: String,
    cube: u64,
    info: std::sync::Arc<janus_cube::CubeInfo>,
    base: Option<&Path>,
) -> crate::zeit::ResultLayer {
    let [x0, y0, x1, y1] = e.window;
    let mut layer = crate::zeit::ResultLayer::for_test(vec![], 0, 0, e.unit.clone());
    layer.cube = cube;
    layer.grid = Some(info);
    layer.source = e.source.clone();
    layer.run = e.run.resolve(base).unwrap_or_else(|| e.run.shown().to_string());
    layer.output = e.output;
    layer.name = e.name.clone();
    layer.path = file;
    layer.cmap = janus_gpu::colormaps::by_name(&e.colormap).unwrap_or(janus_gpu::colormaps::VIRIDIS);
    (layer.x0, layer.y0, layer.w, layer.h) = (x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0));
    (layer.lo, layer.hi, layer.visible, layer.opacity) = (e.lo, e.hi, e.visible, e.opacity);
    layer.classes = e.classes.clone();
    layer
}

/// A Janus project file (by its extension).
pub fn is_project_file(path: &str) -> bool {
    Path::new(path).extension().is_some_and(|e| e.eq_ignore_ascii_case(EXTENSION)) && Path::new(path).is_file()
}

/// The last session, kept at exit and every minute.
pub fn session_path() -> PathBuf {
    crate::settings::app_data_dir().join("last-session.janus")
}

/// A project being opened: its layers one at a time, then the rest.
pub struct Restore {
    project: Project,
    base: Option<PathBuf>,
    /// the series to open next, the one opening (its place and the layers before it)
    next: usize,
    opening: Option<(usize, usize)>,
    /// each series' cube once open
    cubes: Vec<Option<u64>>,
    /// the vector files asked for, their layers read
    vectors_asked: bool,
    results_asked: bool,
    /// the maps asked for: their place in the project, their file
    result_files: Vec<(usize, String)>,
    /// what could not be opened
    pub missing: Vec<String>,
    started: Instant,
}

/// What to do once the changes are saved or discarded.
#[derive(Clone, Debug, PartialEq)]
pub enum After {
    New,
    Open(PathBuf),
}

#[derive(Default)]
pub struct ProjectUi {
    /// the project's file (None: not saved as one)
    pub path: Option<PathBuf>,
    /// the state as saved or opened, and whether it changed since
    saved: String,
    pub dirty: bool,
    /// a session reopened with changes not saved
    forced_dirty: bool,
    checked: Option<Instant>,
    autosaved: Option<Instant>,
    pub restore: Option<Restore>,
    /// the title last given to the window
    title: String,
    /// changes not saved: asked what to do before this
    pub confirm: Option<After>,
    /// the session reopened at the start (once)
    reopened: bool,
    /// a project given on the command line, opened at the first frame
    pub startup: Option<PathBuf>,
}

impl ProjectUi {
    /// With the project given on the command line, if any.
    pub fn starting(startup: Option<PathBuf>) -> ProjectUi {
        ProjectUi { startup, ..Default::default() }
    }

    pub fn restoring(&self) -> bool {
        self.restore.is_some()
    }
}

impl State {
    /// The series' place in the project's list, by its layer index (the canvas left out).
    fn project_series(&self) -> Vec<usize> {
        (0..self.layers.len()).filter(|&i| !self.layers[i].canvas && self.layer_session(i).is_some()).collect()
    }

    /// Everything a project keeps, now; paths relative to `base`.
    pub(crate) fn project_state(&mut self, base: Option<&Path>) -> Project {
        let idx = self.project_series();
        let mut p = Project { format: FORMAT, janus: env!("CARGO_PKG_VERSION").into(), ..Default::default() };
        let mut cube_key = std::collections::HashMap::new();
        for (k, &i) in idx.iter().enumerate() {
            let info = self.layer_session(i).map(|s| s.info.clone()).unwrap();
            cube_key.insert(info.id, k);
            let l = &self.layers[i];
            let t = if i == self.active { self.display.t } else { l.disp.t };
            p.series.push(SeriesEntry {
                name: l.name.clone(),
                inputs: l.inputs.iter().map(|f| FileRef::new(f, base)).collect(),
                selection: Selection::of(&info.sel),
                visible: l.visible,
                style: self.style_of(Sel::Series(info.id)).map(|st| kept_style(st).to_text()).unwrap_or_default(),
                date: info.layers.get(t).map_or(String::new(), |d| d.label.clone()),
                t,
            });
        }
        for v in &self.vectors {
            p.vectors.push(VectorEntry {
                file: FileRef::new(&v.data.path, base),
                layer: v.data.layer.clone(),
                name: v.name.clone(),
                visible: v.visible,
                color: v.color.to_array(),
            });
        }
        let mut result_key = std::collections::HashMap::new();
        for r in self.zeit.results.iter().filter(|r| !r.preview) {
            let Some(&series) = cube_key.get(&r.cube) else { continue };
            result_key.insert(r.id, p.results.len());
            p.results.push(ResultEntry {
                series,
                file: FileRef::new(&r.path, base),
                run: FileRef::new(&r.run, base),
                output: r.output,
                name: r.name.clone(),
                source: r.source.clone(),
                unit: r.unit.clone(),
                colormap: janus_gpu::colormaps::all()[r.cmap].name.to_string(),
                lo: r.lo,
                hi: r.hi,
                visible: r.visible,
                opacity: r.opacity,
                classes: r.classes.clone(),
                window: [r.x0, r.y0, r.x0 + r.w, r.y0 + r.h],
            });
        }
        let key = |s: &State, sel: Sel| -> Option<String> {
            match sel {
                Sel::Series(id) => cube_key.get(&id).map(|k| format!("s{k}")),
                Sel::Vector(id) => s.vectors.iter().position(|v| v.id == id).map(|k| format!("v{k}")),
                Sel::Result(id) => result_key.get(&id).map(|k| format!("r{k}")),
            }
        };
        p.order = self.draw_order().into_iter().filter_map(|it| self.sel_of(it)).filter_map(|s| key(self, s)).collect();
        p.groups = self
            .groups
            .iter()
            .map(|g| GroupEntry {
                name: g.name.clone(),
                members: g.members.iter().filter_map(|&m| key(self, m)).collect(),
            })
            .filter(|g| !g.members.is_empty())
            .collect();
        p.active = idx.iter().position(|&i| i == self.active);
        p.pins = self.series.pins.iter().map(|q| [q.x, q.y]).collect();
        p.pin_classes =
            self.series.pins.iter().map(|q| self.ml.pin_classes.get(&q.id).cloned().unwrap_or_default()).collect();
        p.pin_class_list = self.ml.pin_class_list.clone();
        p.roi = self.series.roi_rect;
        p.roi_polygon = self.series.roi_poly.clone();
        if self.transect.on {
            p.transect = self.transect.pts.clone();
        }
        p.view = self.current_view("project").map(|v| v.to_text()).unwrap_or_default();
        if let Some(sw) = &self.swipe {
            p.swipe = Some(SwipeEntry {
                series: (!sw.view.basemap_only).then(|| cube_key.get(&sw.view.cube).copied()).flatten(),
                x: sw.x,
                lens: sw.lens,
                radius: sw.radius,
                own_date: sw.view.own_date,
                t: sw.view.t,
            });
        }
        p.map_views = self
            .views
            .iter()
            .filter_map(|v| Some(MapViewEntry { series: *cube_key.get(&v.cube)?, own_date: v.own_date, t: v.t }))
            .collect();
        let b = &self.basemap.ui;
        p.basemap = BasemapEntry {
            source: b.source.clone(),
            on: b.on,
            opacity: b.opacity,
            url: b.url.clone(),
            max_zoom: b.max_zoom,
            tile_size: b.tile_size,
            attribution: b.attribution.clone(),
        };
        p.saved_views = self.settings.s.saved_views.clone();
        let s = self.display.season;
        p.season = (s.on, s.start, s.end);
        p
    }

    /// The state a project keeps, without the view, as text (the tests compare a project saved and opened).
    pub(crate) fn project_snapshot(&mut self) -> String {
        let mut p = self.project_state(None);
        p.view.clear();
        serde_json::to_string_pretty(&p).unwrap_or_default()
    }

    /// Every layer closed (series, vector layers, maps, groups), the pins, ROI, transect, swipe and map panels gone.
    pub(crate) fn close_everything(&mut self, ctx: &egui::Context) {
        self.undo.clear();
        self.drop_preview();
        self.close_all(ctx);
        while !self.vectors.is_empty() {
            self.remove_vector(self.vectors.len() - 1);
        }
        self.zeit.results.clear();
        self.groups.clear();
        self.clear_pins();
        self.clear_roi();
        self.clear_transect();
        if self.swipe.is_some() {
            self.toggle_swipe();
        }
        for id in self.views.iter().map(|v| v.id).collect::<Vec<_>>() {
            self.close_map_view(id);
            self.views_closed.push(id);
        }
        self.contents.selected.clear();
        self.map.dirty = true;
    }

    /// File > New project: everything closed, no project's file.
    pub(crate) fn new_project(&mut self, ctx: &egui::Context) {
        if self.project.dirty && self.project.confirm.is_none() {
            self.project.confirm = Some(After::New);
            return;
        }
        self.project.restore = None;
        self.close_everything(ctx);
        self.project.path = None;
        self.project.forced_dirty = false;
        self.project.saved = self.project_state(None).fingerprint();
        self.project.dirty = false;
    }

    /// File > Open project...: a file picked (changes not saved asked about first).
    pub(crate) fn open_project_dialog(&mut self, ctx: &egui::Context) {
        if let Some(p) = rfd::FileDialog::new().add_filter("Janus project", &[EXTENSION]).pick_file() {
            self.open_project(&p, ctx);
        }
    }

    /// A project opened: what is open closed, its layers opened one at a time (pump_project).
    pub(crate) fn open_project(&mut self, path: &Path, ctx: &egui::Context) {
        if self.project.dirty && self.project.confirm.is_none() {
            self.project.confirm = Some(After::Open(path.to_path_buf()));
            return;
        }
        match Project::read(path) {
            Ok(p) => {
                self.start_restore(p, path.parent().map(Path::to_path_buf), ctx);
                self.project.path = Some(path.to_path_buf());
                self.project.forced_dirty = false;
                self.remember_project(path);
            }
            Err(e) => self.error_popup = Some(e),
        }
    }

    fn start_restore(&mut self, p: Project, base: Option<PathBuf>, ctx: &egui::Context) {
        self.close_everything(ctx);
        let n = p.series.len();
        self.project.restore = Some(Restore {
            project: p,
            base,
            next: 0,
            opening: None,
            cubes: vec![None; n],
            vectors_asked: false,
            results_asked: false,
            result_files: vec![],
            missing: vec![],
            started: Instant::now(),
        });
    }

    /// File > Save project (Ctrl+S): to its file, or Save as... when it has none.
    pub(crate) fn save_project(&mut self) {
        if self.project.restoring() {
            return; // half open: nothing to save yet
        }
        match self.project.path.clone() {
            Some(p) => self.save_project_to(&p),
            None => self.save_project_as(),
        }
    }

    pub(crate) fn save_project_as(&mut self) {
        let name = self.project.path.as_ref().and_then(|p| p.file_name()).map_or_else(
            || format!("{}.{EXTENSION}", self.layers.get(self.active).map_or("project".into(), |l| l.name.clone())),
            |n| n.to_string_lossy().into_owned(),
        );
        if let Some(p) =
            rfd::FileDialog::new().add_filter("Janus project", &[EXTENSION]).set_file_name(name).save_file()
        {
            let p = if p.extension().is_none() { p.with_extension(EXTENSION) } else { p };
            self.save_project_to(&p);
        }
    }

    /// The project written to `path` (paths relative to its folder); it becomes the project's file.
    pub(crate) fn save_project_to(&mut self, path: &Path) {
        let base = path.parent().map(Path::to_path_buf);
        let p = self.project_state(base.as_deref());
        match p.write(path) {
            Ok(()) => {
                self.project.path = Some(path.to_path_buf());
                self.project.saved = p.fingerprint();
                (self.project.dirty, self.project.forced_dirty) = (false, false);
                self.remember_project(path);
            }
            Err(e) => self.error_popup = Some(e),
        }
    }

    fn remember_project(&mut self, path: &Path) {
        let p = path.to_string_lossy().into_owned();
        let r = &mut self.settings.s.recent_projects;
        r.retain(|x| *x != p);
        r.insert(0, p);
        r.truncate(10);
        self.settings.changed();
    }

    /// File's project items.
    pub(crate) fn project_menu(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if ui.button("New project").on_hover_text("Close every layer and start again").clicked() {
            self.new_project(&ctx);
            ui.close();
        }
        if ui.button("Open project...").clicked() {
            ui.close();
            self.open_project_dialog(&ctx);
        }
        if ui.add(egui::Button::new("Save project").shortcut_text("Ctrl+S")).clicked() {
            ui.close();
            self.save_project();
        }
        if ui.add(egui::Button::new("Save project as...")).clicked() {
            ui.close();
            self.save_project_as();
        }
        let recent = self.settings.s.recent_projects.clone();
        ui.add_enabled_ui(!recent.is_empty(), |ui| {
            ui.menu_button("Recent projects", |ui| {
                for r in &recent {
                    let name = Path::new(r).file_name().map_or(r.clone(), |n| n.to_string_lossy().into_owned());
                    if ui.button(name).on_hover_text(r).clicked() {
                        ui.close();
                        self.open_project(Path::new(r), &ctx);
                    }
                }
            });
        });
    }

    /// Ctrl+S: Save project.
    pub(crate) fn project_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::S)) {
            self.save_project();
        }
    }

    /// Each frame: a project being opened goes on; whether the project changed (once a second); the session kept
    /// every minute and at exit; the window's title; the session reopened at the start.
    pub(crate) fn pump_project(&mut self, ctx: &egui::Context) {
        if !self.project.reopened {
            self.project.reopened = true;
            if let Some(p) = self.project.startup.take() {
                self.open_project(&p, ctx);
            }
            let interactive = self.opts.inputs.is_empty() && !self.opts.scripted() && self.opts.screenshot.is_none();
            if interactive && self.settings.s.reopen_last && !self.settings.no_save {
                self.reopen_session(ctx);
            }
        }
        if self.project.restore.is_some() {
            self.restore_step(ctx);
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
            return;
        }
        let now = Instant::now();
        if self.project.checked.is_none_or(|t| (now - t).as_secs_f64() >= 1.0) {
            self.project.checked = Some(now);
            let fp = self.project_state(None).fingerprint();
            if self.project.saved.is_empty() {
                // the state at the start is the one "saved"
                self.project.saved = fp.clone();
            }
            self.project.dirty = self.project.forced_dirty || fp != self.project.saved;
        }
        let closing = ctx.input(|i| i.viewport().close_requested());
        let keep = !self.settings.no_save && !self.opts.scripted() && self.opts.screenshot.is_none();
        if keep && (closing || self.project.autosaved.is_none_or(|t| (now - t).as_secs_f64() >= 60.0)) {
            self.project.autosaved = Some(now);
            self.save_session();
        }
        let title = self.window_title();
        if title != self.project.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.project.title = title;
        }
    }

    /// "Janus - project* - the active layer (its description)".
    pub(crate) fn window_title(&self) -> String {
        let mut parts = vec!["Janus".to_string()];
        if let Some(p) = &self.project.path {
            let name = p.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
            parts.push(format!("{name}{}", if self.project.dirty { "*" } else { "" }));
        }
        if let (Some(l), Some(s)) = (self.layers.get(self.active), &self.session) {
            let n = self.layers.len();
            parts.push(format!(
                "{} ({}){}",
                l.name,
                s.info.description,
                if n > 1 { format!("  [{n} layers]") } else { String::new() }
            ));
        }
        parts.join(" - ")
    }

    /// The session (absolute paths) in the settings' folder; none when nothing is open.
    fn save_session(&mut self) {
        let mut p = self.project_state(None);
        if p.series.is_empty() && p.vectors.is_empty() {
            let _ = std::fs::remove_file(session_path());
            return;
        }
        p.project_file = self.project.path.as_ref().map_or(String::new(), |x| x.to_string_lossy().into_owned());
        p.unsaved = self.project.dirty;
        let _ = std::fs::create_dir_all(crate::settings::app_data_dir());
        let _ = p.write(&session_path());
    }

    /// The last session opened again (its project's file, if it was one, named in the title).
    fn reopen_session(&mut self, ctx: &egui::Context) {
        let Ok(p) = Project::read(&session_path()) else { return };
        if p.series.is_empty() && p.vectors.is_empty() {
            return;
        }
        let file = (!p.project_file.is_empty()).then(|| PathBuf::from(&p.project_file));
        let unsaved = p.unsaved || file.is_none();
        self.start_restore(p, None, ctx);
        self.project.path = file.filter(|f| f.exists());
        self.project.forced_dirty = unsaved && self.project.path.is_some();
    }

    /// One step of a project being opened: the next series, its state once it is open; then the vector layers,
    /// the runs' maps, and the rest.
    fn restore_step(&mut self, ctx: &egui::Context) {
        if self.opening.is_some() {
            return;
        }
        let Some(mut r) = self.project.restore.take() else { return };
        // a series asked for: open (applied) or not
        if let Some((k, before)) = r.opening.take() {
            if self.layers.len() > before {
                let i = self.layers.len() - 1;
                let e = r.project.series[k].clone();
                r.cubes[k] = self.layer_session(i).map(|s| s.info.id);
                self.apply_series_entry(i, &e, ctx);
            } else {
                r.missing.push(format!("{} (it could not be opened)", r.project.series[k].name));
                self.error = None;
            }
        }
        if r.next < r.project.series.len() {
            let k = r.next;
            r.next += 1;
            let e = &r.project.series[k];
            let inputs: Vec<String> = e.inputs.iter().filter_map(|f| f.resolve(r.base.as_deref())).collect();
            if inputs.len() < e.inputs.len() {
                let gone: Vec<&str> =
                    e.inputs.iter().filter(|f| f.resolve(r.base.as_deref()).is_none()).map(|f| f.shown()).collect();
                r.missing.push(format!("{}: {} not found", e.name, gone.join(", ")));
            }
            if !inputs.is_empty() {
                let mut o = crate::session::Opening::start_with(
                    inputs,
                    !self.layers.is_empty(),
                    e.selection.band_selection(),
                    ctx,
                );
                o.name = Some(e.name.clone());
                self.opening = Some(o);
                r.opening = Some((k, self.layers.len()));
            }
            self.project.restore = Some(r);
            return;
        }
        // the vector layers, all at once
        if !r.vectors_asked {
            r.vectors_asked = true;
            let mut files: Vec<String> = vec![];
            for v in &r.project.vectors {
                match v.file.resolve(r.base.as_deref()) {
                    Some(f) if !files.contains(&f) => files.push(f),
                    Some(_) => {}
                    None => r.missing.push(format!("{}: {} not found", v.name, v.file.shown())),
                }
            }
            if !files.is_empty() {
                self.add_vector_files(files, ctx);
            }
            self.project.restore = Some(r);
            return;
        }
        if !self.vector_loads.is_empty() {
            self.project.restore = Some(r);
            return;
        }
        // the runs' maps, read as a run's are
        if !r.results_asked {
            r.results_asked = true;
            for (k, e) in r.project.results.iter().enumerate() {
                let Some(file) = e.file.resolve(r.base.as_deref()) else {
                    r.missing.push(format!("{}: {} not found", e.name, e.file.shown()));
                    continue;
                };
                let grid = r.cubes.get(e.series).copied().flatten().and_then(|c| {
                    let i = (0..self.layers.len()).find(|&i| self.layer_session(i).is_some_and(|s| s.info.id == c))?;
                    self.layer_session(i).map(|s| (c, s.info.clone()))
                });
                let Some((cube, info)) = grid else {
                    r.missing.push(format!("{}: its series is not open", e.name));
                    continue;
                };
                let layer = result_layer(e, file, cube, info, r.base.as_deref());
                r.result_files.push((k, layer.path.clone()));
                self.load_result_layer(layer, false, ctx);
            }
            self.project.restore = Some(r);
            return;
        }
        if self.zeit.loads_pending() {
            self.project.restore = Some(r);
            return;
        }
        self.finish_restore(r, ctx);
    }

    /// A series' state as the project had it (it has just opened: the active one).
    pub(crate) fn apply_series_entry(&mut self, i: usize, e: &SeriesEntry, ctx: &egui::Context) {
        if i != self.active {
            self.set_active(i, ctx);
        }
        let Some(id) = self.layer_session(i).map(|s| s.info.id) else { return };
        if let Some(st) = crate::style::LayerStyle::from_text(&e.style) {
            self.apply_style(Sel::Series(id), &st);
        }
        let n = self.layer_session(i).map_or(1, |s| s.info.t());
        let t = self
            .layer_session(i)
            .and_then(|s| s.info.layers.iter().position(|l| l.label == e.date))
            .unwrap_or(e.t.min(n.saturating_sub(1)));
        self.set_date(t);
        let l = &mut self.layers[i];
        (l.name, l.visible) = (e.name.clone(), e.visible);
    }

    /// The rest, once every layer is open: the order, groups, active layer, pins, ROI, transect, view, swipe, map
    /// panels, basemap, saved views, season; what was missing said in a notice.
    fn finish_restore(&mut self, r: Restore, ctx: &egui::Context) {
        let p = &r.project;
        // the items by the project's keys
        let mut items: std::collections::HashMap<String, Item> = std::collections::HashMap::new();
        for (k, c) in r.cubes.iter().enumerate() {
            if let Some(i) =
                c.and_then(|c| (0..self.layers.len()).find(|&i| self.layer_session(i).is_some_and(|s| s.info.id == c)))
            {
                items.insert(format!("s{k}"), Item::Series(i));
            }
        }
        // vector layers by file and layer, the extra layers of a file (not in the project) closed
        let mut taken = vec![false; self.vectors.len()];
        for (k, e) in p.vectors.iter().enumerate() {
            let file = e.file.resolve(r.base.as_deref());
            let found = (0..self.vectors.len()).find(|&j| {
                !taken[j] && Some(&self.vectors[j].data.path) == file.as_ref() && self.vectors[j].data.layer == e.layer
            });
            if let Some(j) = found {
                taken[j] = true;
                let v = &mut self.vectors[j];
                (v.name, v.visible) = (e.name.clone(), e.visible);
                v.color = egui::Color32::from_rgba_premultiplied(e.color[0], e.color[1], e.color[2], e.color[3]);
                items.insert(format!("v{k}"), Item::Vector(j));
            }
        }
        // the maps read, by their files
        for (k, path) in &r.result_files {
            if let Some(j) = self.zeit.results.iter().position(|x| &x.path == path) {
                items.insert(format!("r{k}"), Item::Result(j));
            }
        }
        let extra: Vec<usize> = (0..self.vectors.len()).filter(|&j| !taken[j]).collect();
        // the drawing order: the project's, any other layer kept on top
        let mut order: Vec<Item> = p.order.iter().filter_map(|k| items.get(k).copied()).collect();
        for it in self.draw_order() {
            if !order.contains(&it) && !matches!(it, Item::Vector(j) if extra.contains(&j)) {
                order.push(it);
            }
        }
        // the Sel of each key (stable through the order's sorting)
        let sels: std::collections::HashMap<String, Sel> =
            items.iter().filter_map(|(k, &it)| Some((k.clone(), self.sel_of(it)?))).collect();
        let extra_ids: Vec<u32> = extra.iter().map(|&j| self.vectors[j].id).collect();
        let active = p.active.and_then(|k| sels.get(&format!("s{k}")).copied());
        // the canvas (vector layers alone) stays at the bottom
        if let Some(c) = self.canvas_index() {
            order.retain(|it| *it != Item::Series(c));
            order.insert(0, Item::Series(c));
        }
        let order: Vec<Item> =
            order.into_iter().filter(|it| !matches!(it, Item::Vector(j) if extra.contains(j))).collect();
        // the extra vector layers' places: the order without them, then they go
        let mut full = order.clone();
        for &j in &extra {
            full.push(Item::Vector(j));
        }
        self.set_draw_order(&full);
        for id in extra_ids {
            if let Some(j) = self.vector_index(id) {
                self.remove_vector(j);
            }
        }
        for g in &p.groups {
            let members: Vec<Sel> = g.members.iter().filter_map(|k| sels.get(k).copied()).collect();
            if members.len() >= 2 {
                self.next_group += 1;
                self.groups.push(crate::groups::LayerGroup { id: self.next_group, name: g.name.clone(), members });
            }
        }
        if let Some(Sel::Series(id)) = active
            && let Some(i) = self.series_index(id)
            && i != self.active
        {
            self.set_active(i, ctx);
        }
        if let Some(e) = p.active.and_then(|k| p.series.get(k))
            && let Some(t) = self.session.as_ref().and_then(|s| s.info.layers.iter().position(|l| l.label == e.date))
        {
            self.set_date(t);
        }
        // the season and the basemap
        let (on, start, end) = p.season;
        self.display.season = crate::season::Season { on, start, end };
        let b = &mut self.basemap.ui;
        if !p.basemap.source.is_empty() {
            (b.source, b.on, b.opacity) = (p.basemap.source.clone(), p.basemap.on, p.basemap.opacity);
            (b.url, b.max_zoom, b.tile_size) = (p.basemap.url.clone(), p.basemap.max_zoom, p.basemap.tile_size);
            b.attribution = p.basemap.attribution.clone();
        }
        // the view, then the marks in the active layer's pixels
        if let Some(v) = crate::navigation::SavedView::from_text(&p.view) {
            let _ = self.restore_view(&v);
        }
        self.restore_pins(&p.pins, &p.pin_classes, &p.pin_class_list);
        if let Some([x0, y0, x1, y1]) = p.roi {
            self.set_roi_shape([x0 as i64, y0 as i64, x1 as i64, y1 as i64], p.roi_polygon.clone());
        }
        if p.transect.len() >= 2 {
            self.set_transect(p.transect.clone());
        }
        let cube =
            |k: usize| sels.get(&format!("s{k}")).and_then(|s| if let Sel::Series(id) = s { Some(*id) } else { None });
        if let Some(sw) = &p.swipe {
            self.toggle_swipe();
            if let Some(s) = self.swipe.as_mut() {
                (s.x, s.lens, s.radius, s.lens_only) = (sw.x, sw.lens, sw.radius, sw.lens);
                match sw.series.and_then(cube) {
                    Some(c) => (s.view.cube, s.view.basemap_only) = (c, false),
                    None => s.view.basemap_only = sw.series.is_none(),
                }
                (s.view.own_date, s.view.t, s.view.dirty) = (sw.own_date, sw.t, true);
            }
        }
        for mv in &p.map_views {
            let Some(c) = cube(mv.series) else { continue };
            self.new_map_view();
            if let Some(v) = self.views.last_mut() {
                (v.cube, v.own_date, v.t, v.dirty) = (c, mv.own_date, mv.t, true);
            }
        }
        // saved views: those of the project not among the settings' added
        let names: Vec<String> = self.saved_views().into_iter().map(|v| v.name).collect();
        let mut added = false;
        for t in &p.saved_views {
            if crate::navigation::SavedView::from_text(t).is_some_and(|v| !names.contains(&v.name)) {
                self.settings.s.saved_views.push(t.clone());
                added = true;
            }
        }
        if added {
            self.settings.changed();
        }
        self.map.dirty = true;
        let took = r.started.elapsed().as_secs_f64();
        if !r.missing.is_empty() {
            let text = r.missing.join("\n");
            self.notify("Project opened without some of its layers", &text, true);
        }
        let n = self.project_series().len();
        println_restore(n, self.vectors.len(), self.zeit.results.len(), took);
        self.project.saved = self.project_state(None).fingerprint();
        self.project.dirty = self.project.forced_dirty;
        self.project.restore = None;
        // the history starts from the project as opened
        self.undo.clear();
    }

    /// The project's window of changes not saved: Save, Discard or Cancel before a new project or another one.
    pub(crate) fn project_confirm_window(&mut self, ctx: &egui::Context) {
        let Some(after) = self.project.confirm.clone() else { return };
        let name = self
            .project
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map_or("The project".to_string(), |n| n.to_string_lossy().into_owned());
        let mut choice = None;
        egui::Window::new("Changes not saved")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(format!("{name} has changes not saved."));
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        choice = Some(0);
                    }
                    if ui.button("Discard").clicked() {
                        choice = Some(1);
                    }
                    if ui.button("Cancel").clicked() {
                        choice = Some(2);
                    }
                });
            });
        let Some(c) = choice else { return };
        if c == 0 {
            self.save_project();
            if self.project.dirty {
                self.project.confirm = None;
                return; // not saved (the dialog cancelled)
            }
        }
        if c == 2 {
            self.project.confirm = None;
            return;
        }
        self.project.dirty = false;
        self.project.forced_dirty = false;
        match after {
            After::New => self.new_project(ctx),
            After::Open(p) => self.open_project(&p, ctx),
        }
        self.project.confirm = None;
    }
}

/// The time a project took to open, for the log.
fn println_restore(series: usize, vectors: usize, maps: usize, took: f64) {
    eprintln!("project opened: {series} series, {vectors} vector layers, {maps} maps in {took:.1} s");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths() {
        let base = if cfg!(windows) { Path::new("C:\\data\\proj") } else { Path::new("/data/proj") };
        let p = |s: &str| {
            if cfg!(windows) { PathBuf::from(format!("C:{}", s.replace('/', "\\"))) } else { PathBuf::from(s) }
        };
        assert_eq!(relative(&p("/data/proj/a/x.tif"), base).as_deref(), Some("a/x.tif"));
        assert_eq!(relative(&p("/data/other/y.tif"), base).as_deref(), Some("../other/y.tif"));
        assert_eq!(relative(&p("/data/proj"), base).as_deref(), Some("."));
        if cfg!(windows) {
            assert_eq!(relative(Path::new("D:\\x.tif"), base), None);
        }
        // a URL or a GDAL name stays as it is
        let f = FileRef::new("https://example.com/a.tif", Some(base));
        assert_eq!(f.resolve(Some(base)).as_deref(), Some("https://example.com/a.tif"));
    }

    #[test]
    fn project_round_trip_and_selection() {
        let mut p = Project { format: FORMAT, ..Default::default() };
        let sel = janus_cube::BandSelection {
            band: 4,
            nd_band: 3,
            qa_band: 5,
            qa_rule: janus_cube::QaRule::Fmask,
            chosen: true,
            ..Default::default()
        };
        p.series.push(SeriesEntry { name: "a".into(), selection: Selection::of(&sel), ..Default::default() });
        let dir = std::env::temp_dir().join(format!("janus-project-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("t.janus");
        p.write(&f).unwrap();
        let q = Project::read(&f).unwrap();
        assert_eq!(q, p);
        assert_eq!(q.series[0].selection.band_selection(), sel);
        // the view is not a change
        let mut r = q.clone();
        r.view = "elsewhere".into();
        assert_eq!(r.fingerprint(), q.fingerprint());
        std::fs::write(&f, "{}").unwrap();
        assert!(Project::read(&f).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
