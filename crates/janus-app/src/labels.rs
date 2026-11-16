//! Interpretation (src/app_labels.cpp): a vector layer's features gone through one by one, as TimeSync does with
//! Landsat: the map centred on each (zoomed in), its series on the chart (a pin at a point; a polygon becomes the
//! ROI), a label from the label set (keys 1-9, each with its own colour), the date of the change (D: the date on the
//! map) and notes, its confidence (Q W E) and more events (Shift+E). Labels are saved as they are given (in the app data, by file and layer: opening
//! the same file again resumes the work) and exported as a table (CSV, GeoPackage, GeoJSON, Shapefile) with the
//! file's own fields. A validation project's sample saves them in its GeoPackage instead (validation.rs), under
//! the interpreter's name, and can be gone through by a filter (a cell of the error matrix, the disagreements).
//!
//! The interpretation panel's cards, keys and navigation follow Validation Viewer by Dr. Matthias Baumann
//! (Humboldt-Universität zu Berlin), https://scm.cms.hu-berlin.de/baumamat/validationviewer, GPL-2.0-or-later, used
//! under GPL-3.0-or-later.

use std::sync::Arc;
use std::time::Instant;

use egui::{Color32, CornerRadius, Margin, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use janus_geo::vectors::{Kind, write_vector_table};
use serde_json::{Value, json};

use crate::app::State;
use crate::exports::{ExportJob, start_export_job};

const DEFAULT_CLASSES: [&str; 9] = [
    "Stable forest",
    "Deforestation",
    "Degradation",
    "Regeneration",
    "Agriculture",
    "Pasture",
    "Water",
    "Cloud / no data",
    "Other",
];

/// The colours of the nine default labels: forest green, red, orange, light green, yellow, khaki, blue, grey,
/// purple.
const DEFAULT_COLORS: [[u8; 3]; 9] = [
    [39, 140, 74],
    [214, 58, 52],
    [240, 140, 30],
    [140, 205, 90],
    [245, 210, 50],
    [180, 170, 100],
    [50, 120, 220],
    [140, 145, 155],
    [140, 80, 190],
];

/// The categorical colours given to the labels added to a set, in turn.
const EXTRA_COLORS: [[u8; 3]; 8] = [
    [0, 150, 160],
    [225, 100, 160],
    [140, 90, 60],
    [20, 190, 220],
    [190, 60, 150],
    [100, 110, 220],
    [200, 130, 110],
    [120, 160, 40],
];

/// The colour of label `name`, the k-th of its set: the default one of a default label, else the next of the
/// categorical palette.
pub fn default_class_color(name: &str, k: usize) -> [u8; 3] {
    match DEFAULT_CLASSES.iter().position(|c| c.eq_ignore_ascii_case(name)) {
        Some(i) => DEFAULT_COLORS[i],
        None => EXTRA_COLORS[k % EXTRA_COLORS.len()],
    }
}

pub fn color_hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

pub fn parse_color_hex(s: &str) -> Option<[u8; 3]> {
    let h = s.trim().strip_prefix('#')?;
    if h.len() != 6 || !h.is_ascii() {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

/// `hex` (as stored, "#rrggbb"; fewer than `classes` or damaged: the defaults) as one colour per class.
pub fn colors_from_hex(classes: &[String], hex: &[String]) -> Vec<[u8; 3]> {
    classes
        .iter()
        .enumerate()
        .map(|(k, c)| hex.get(k).and_then(|h| parse_color_hex(h)).unwrap_or_else(|| default_class_color(c, k)))
        .collect()
}

/// A feature's label and its events (the validation's: a project keeps the same).
pub use janus_validation::{Event, Label, now_iso};

/// The interpretation going on.
pub struct Interpretation {
    /// the vector layer's id (None: none)
    pub vector: Option<u32>,
    /// the feature shown
    pub current: Option<usize>,
    /// the label set (keys 1-9)
    pub classes: Vec<String>,
    /// the colour of each class (as long as `classes`)
    pub colors: Vec<[u8; 3]>,
    /// when the labels were last written
    pub saved: Option<Instant>,
    /// one per feature
    pub entries: Vec<Label>,
    /// to the next feature after a label
    pub advance: bool,
    /// N / Shift+N skip labelled features
    pub skip_labelled: bool,
    /// at least this many screen points per pixel
    pub zoom: f32,
    /// the pin at the current feature
    pub pin: Option<u32>,
    /// saved a second after the last change
    pub dirty: bool,
    changed_at: Option<Instant>,
    pub notes: String,
    new_class: String,
    /// the label of an event being added ("" = the feature's)
    pub event_label: String,
    scroll_to_current: bool,
    /// the Interpretation panel is shown
    pub show: bool,
    /// the sample of a validation project: labels kept in it, as `who`
    pub project: bool,
    pub who: String,
    /// only these features, in this order (empty: all), and what they are
    pub filter: Vec<usize>,
    pub filter_name: String,
}

impl Default for Interpretation {
    fn default() -> Self {
        Interpretation {
            vector: None,
            current: None,
            classes: Vec::new(),
            colors: Vec::new(),
            saved: None,
            entries: Vec::new(),
            advance: false,
            skip_labelled: false,
            zoom: 4.0,
            pin: None,
            dirty: false,
            changed_at: None,
            notes: String::new(),
            new_class: String::new(),
            event_label: String::new(),
            scroll_to_current: false,
            show: false,
            project: false,
            who: String::new(),
            filter: Vec::new(),
            filter_name: String::new(),
        }
    }
}

impl State {
    pub fn start_interpretation(&mut self, id: u32) {
        self.labels.show = true;
        if self.labels.vector == Some(id) {
            return;
        }
        let Some(li) = self.vector_index(id) else { return };
        self.stop_interpretation();
        let n = self.vectors[li].data.features.len();
        let l = &mut self.labels;
        l.vector = Some(id);
        l.classes = DEFAULT_CLASSES.iter().map(|s| s.to_string()).collect();
        l.colors = colors_from_hex(&l.classes, &[]);
        l.entries = vec![Label::default(); n];
        // the dock arranged once per run for it (not in the self-test or the tours, which place their own panels)
        if !self.interp_layout_done && !self.opts.selftest_ui && self.shots.is_none() {
            self.interp_layout_done = true;
            self.want_interp_layout = true;
        }
        if self.is_project_vector(id) {
            self.labels.project = true;
            self.labels.who = self.interpreter_name();
            self.validation_entries();
        } else {
            self.load_labels();
        }
        // where the work stopped: the first feature without a label
        let d = &self.vectors[li].data;
        let has = |i: usize| d.features[i].kind != Kind::None;
        let first =
            (0..n).find(|&i| has(i) && self.labels.entries[i].label.is_empty()).or_else(|| (0..n).find(|&i| has(i)));
        if let Some(i) = first {
            self.interp_go(i);
        }
    }

    pub fn stop_interpretation(&mut self) {
        if self.labels.vector.is_none() {
            return;
        }
        if self.labels.dirty {
            self.save_labels();
        }
        if let Some(p) = self.labels.pin {
            self.remove_pin(p);
        }
        let show = self.labels.show;
        self.labels = Interpretation { show, ..Default::default() };
    }

    fn interp_layer(&self) -> Option<usize> {
        self.labels.vector.and_then(|id| self.vector_index(id))
    }

    /// Feature i shown: a polygon as the ROI, anything else with a pin at its point; the map centred on it.
    pub fn interp_go(&mut self, i: usize) {
        let Some(li) = self.interp_layer() else { return };
        let Some(info) = self.session.as_ref().map(|s| s.info.clone()) else { return };
        let d = self.vectors[li].data.clone();
        if d.features.get(i).is_none_or(|f| f.kind == Kind::None) {
            return;
        }
        // the previous feature's polygon was made the ROI here: it goes with it
        if self.labels.current.is_some_and(|c| d.features.get(c).is_some_and(|f| f.kind == Kind::Area)) {
            self.clear_roi();
        }
        let l = &mut self.labels;
        l.current = Some(i);
        l.notes = l.entries[i].notes.clone();
        l.scroll_to_current = true;
        if let Some(p) = l.pin.take() {
            self.remove_pin(p);
        }
        if d.features[i].kind != Kind::Area || !self.feature_as_roi(li, i) {
            let a = self.vectors[li].anchor.get(i).copied().unwrap_or([f64::NAN; 2]);
            if a[0] >= 0.0 && a[1] >= 0.0 && a[0] < info.width as f64 && a[1] < info.height as f64 {
                self.add_pin(a[0] as usize, a[1] as usize);
                self.labels.pin = self.series.pins.last().map(|p| p.id);
            }
        }
        self.center_on_feature(li, i, f64::from(self.labels.zoom));
    }

    /// The next (+1) or previous (-1) feature with a geometry (unlabelled ones only with "Skip labelled"; only
    /// the filter's, in its order, with one).
    pub fn interp_step(&mut self, dir: i64) {
        let Some(li) = self.interp_layer() else { return };
        let d = self.vectors[li].data.clone();
        if !self.labels.filter.is_empty() {
            let f = self.labels.filter.clone();
            let pos = match self.labels.current.and_then(|c| f.iter().position(|&x| x == c)) {
                Some(p) => p as i64,
                None if dir > 0 => -1,
                None => f.len() as i64,
            };
            let mut k = pos + dir;
            while (0..f.len() as i64).contains(&k) {
                let i = f[k as usize];
                if i < self.labels.entries.len()
                    && d.features[i].kind != Kind::None
                    && !(self.labels.skip_labelled && !self.labels.entries[i].label.is_empty())
                {
                    self.interp_go(i);
                    return;
                }
                k += dir;
            }
            return;
        }
        let n = d.features.len() as i64;
        let mut i = self.labels.current.map_or(-1, |c| c as i64) + dir;
        while (0..n).contains(&i) {
            let k = i as usize;
            if d.features[k].kind != Kind::None
                && !(self.labels.skip_labelled && !self.labels.entries[k].label.is_empty())
            {
                self.interp_go(k);
                return;
            }
            i += dir;
        }
    }

    /// Class k of the label set (None: cleared).
    pub fn interp_label(&mut self, k: Option<usize>) {
        let Some(c) = self.labels.current.filter(|&c| c < self.labels.entries.len()) else { return };
        if k.is_some_and(|k| k >= self.labels.classes.len()) {
            return;
        }
        self.labels.entries[c].label = k.map_or(String::new(), |k| self.labels.classes[k].clone());
        self.interp_touched();
        if k.is_some() && self.labels.advance {
            self.interp_step(1);
        }
    }

    /// The date of the change: the map's (or none).
    pub fn interp_set_date(&mut self, clear: bool) {
        let Some(s) = &self.session else { return };
        let Some(c) = self.labels.current.filter(|&c| c < self.labels.entries.len()) else { return };
        self.labels.entries[c].date = if clear { String::new() } else { s.info.layers[self.display.t].label.clone() };
        self.interp_touched();
    }

    pub(crate) fn interp_set_confidence(&mut self, v: i32) {
        let Some(c) = self.labels.current.filter(|&c| c < self.labels.entries.len()) else { return };
        self.labels.entries[c].confidence = v.clamp(0, 3);
        self.interp_touched();
    }

    pub(crate) fn interp_add_event(&mut self, label: String) {
        let Some(s) = &self.session else { return };
        let Some(c) = self.labels.current.filter(|&c| c < self.labels.entries.len()) else { return };
        if label.is_empty() {
            return;
        }
        let date = s.info.layers[self.display.t].label.clone();
        self.labels.entries[c].events.push(Event { label, date });
        self.interp_touched();
    }

    fn interp_touched(&mut self) {
        let l = &mut self.labels;
        let Some(c) = l.current.filter(|&c| c < l.entries.len()) else { return };
        l.entries[c].updated = now_iso();
        l.dirty = true;
        l.changed_at = Some(Instant::now());
        if l.project {
            self.validation_labels_changed(c);
        }
    }

    /// The next feature without a label (of mine, in the filter's order with one), after the current one and
    /// round the end; the panel's "next open" and the O key.
    pub fn interp_next_open(&mut self) {
        let Some(li) = self.interp_layer() else { return };
        let d = self.vectors[li].data.clone();
        let order: Vec<usize> =
            if self.labels.filter.is_empty() { (0..d.features.len()).collect() } else { self.labels.filter.clone() };
        let start = self.labels.current.and_then(|c| order.iter().position(|&x| x == c)).map_or(0, |p| p + 1);
        let open = |i: usize| {
            self.labels.entries.get(i).is_some_and(|e| e.label.is_empty()) && d.features[i].kind != Kind::None
        };
        let n = order.len();
        if let Some(i) = (0..n).map(|k| order[(start + k) % n]).find(|&i| open(i)) {
            self.interp_go(i);
        }
    }

    /// Left / Right the previous / next feature, N / Shift+N (Page Down / Up) too, O the next without a label,
    /// 1-9 a label, 0 clears it, D / Shift+D the date, Q W E (or Shift+1-3) the confidence, Shift+E an event at the
    /// map's date. The date on the map goes by [ and ] (app.rs) meanwhile. Keys with Shift come first: egui
    /// matches a chord whatever Shift is held when the chord does not name it.
    pub(crate) fn interp_shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers};
        if !self.labels.show || self.labels.vector.is_none() {
            return;
        }
        let chord = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_key(m, k));
        const DIGITS: [Key; 10] = [
            Key::Num0,
            Key::Num1,
            Key::Num2,
            Key::Num3,
            Key::Num4,
            Key::Num5,
            Key::Num6,
            Key::Num7,
            Key::Num8,
            Key::Num9,
        ];
        for (c, &key) in DIGITS.iter().enumerate().take(4).skip(1) {
            // low, medium, high confidence
            if chord(Modifiers::SHIFT, key) {
                self.interp_set_confidence(c as i32);
            }
        }
        if chord(Modifiers::SHIFT, Key::E)
            && let Some(c) = self.labels.current.filter(|&c| c < self.labels.entries.len())
        {
            // the label chosen for events, else the feature's
            let l = if self.labels.event_label.is_empty() {
                self.labels.entries[c].label.clone()
            } else {
                self.labels.event_label.clone()
            };
            self.interp_add_event(l);
        }
        if chord(Modifiers::SHIFT, Key::N)
            || chord(Modifiers::NONE, Key::PageUp)
            || chord(Modifiers::NONE, Key::ArrowLeft)
        {
            self.interp_step(-1);
        }
        if chord(Modifiers::NONE, Key::N)
            || chord(Modifiers::NONE, Key::PageDown)
            || chord(Modifiers::NONE, Key::ArrowRight)
        {
            self.interp_step(1);
        }
        if chord(Modifiers::NONE, Key::O) {
            self.interp_next_open();
        }
        if chord(Modifiers::SHIFT, Key::D) {
            self.interp_set_date(true);
        }
        if chord(Modifiers::NONE, Key::D) {
            self.interp_set_date(false);
        }
        for (c, key) in [Key::Q, Key::W, Key::E].into_iter().enumerate() {
            if chord(Modifiers::NONE, key) {
                self.interp_set_confidence(c as i32 + 1);
            }
        }
        for (k, &key) in DIGITS.iter().enumerate().skip(1) {
            if chord(Modifiers::NONE, key) {
                self.interp_label(Some(k - 1));
            }
        }
        if chord(Modifiers::NONE, Key::Num0) {
            self.interp_label(None);
        }
    }

    /// Once a frame: the session ends with its layer; labels saved a second after the last change.
    pub(crate) fn pump_interpretation(&mut self, ctx: &egui::Context) {
        if self.labels.vector.is_some() && self.interp_layer().is_none() {
            self.stop_interpretation();
        }
        if self.labels.dirty {
            if self.labels.changed_at.is_none_or(|t| t.elapsed().as_secs_f64() > 1.0) {
                self.save_labels();
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(300));
            }
        }
    }

    /// Where the labels of the layer are kept: the app data, by file and layer.
    pub fn labels_file(&self) -> Option<std::path::PathBuf> {
        use std::hash::{Hash, Hasher};
        let d = &self.vectors[self.interp_layer()?].data;
        let abs = std::path::absolute(&d.path).map_or(d.path.clone(), |p| p.to_string_lossy().into_owned());
        let mut h = std::collections::hash_map::DefaultHasher::new();
        format!("{abs}|{}", d.layer).hash(&mut h);
        let stem =
            std::path::Path::new(&d.path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let name = format!("{}_{:016x}.json", crate::exports::sanitize(&stem), h.finish());
        Some(crate::settings::app_data_dir().join("labels").join(name))
    }

    pub fn save_labels(&mut self) {
        self.labels.dirty = false;
        self.labels.saved = Some(Instant::now());
        if self.labels.project {
            self.validation_save_labels();
            return;
        }
        let (Some(li), Some(path)) = (self.interp_layer(), self.labels_file()) else { return };
        let d = &self.vectors[li].data;
        let entries: Vec<Value> = self
            .labels
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| !e.is_empty())
            .map(|(i, e)| {
                let mut o = e.to_json();
                o["fid"] = json!(d.features[i].fid);
                o
            })
            .collect();
        let j = json!({
            "path": d.path,
            "layer": d.layer,
            "classes": self.labels.classes,
            "colors": self.labels.colors.iter().map(|&c| color_hex(c)).collect::<Vec<_>>(),
            "entries": entries,
        });
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let text = serde_json::to_string_pretty(&j).unwrap_or_default();
        let _ = crate::exports::write_text_file(&path.to_string_lossy(), &text);
    }

    /// The labels given before (a damaged file: the session starts empty, the file replaced at the first label).
    fn load_labels(&mut self) {
        let (Some(li), Some(path)) = (self.interp_layer(), self.labels_file()) else { return };
        let Ok(text) = std::fs::read_to_string(path) else { return };
        let Ok(j) = serde_json::from_str::<Value>(&text) else { return };
        if let Some(c) = j["classes"].as_array().filter(|c| !c.is_empty()) {
            self.labels.classes = c.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect();
            // files from before the colours have none: the defaults
            let hex: Vec<String> =
                j["colors"].as_array().into_iter().flatten().map(|v| v.as_str().unwrap_or("").to_string()).collect();
            self.labels.colors = colors_from_hex(&self.labels.classes, &hex);
        }
        let d = &self.vectors[li].data;
        let by_fid: std::collections::HashMap<i64, usize> =
            d.features.iter().enumerate().map(|(i, f)| (f.fid, i)).collect();
        for e in j["entries"].as_array().into_iter().flatten() {
            if let Some(&i) = by_fid.get(&e["fid"].as_i64().unwrap_or(-1)) {
                self.labels.entries[i] = Label::from_json(e);
            }
        }
    }

    /// The layer's features and fields with label, change_date and notes, as a table (by extension).
    pub fn export_labels(&mut self, path: String, ctx: &egui::Context) -> Option<Arc<ExportJob>> {
        let li = self.interp_layer()?;
        let v = &self.vectors[li];
        let data = v.data.clone();
        let entries = Arc::new(self.labels.entries.clone());
        let labelled = entries.iter().filter(|e| !e.label.is_empty()).count();
        let title = format!("Labels of {} ({labelled} of {} features)", v.name, entries.len());
        let job = start_export_job(title, path.clone(), ctx, move |j| {
            j.set_progress(0.5);
            let names = ["label", "change_date", "notes"].map(String::from);
            write_vector_table(&path, &data, &names, &|i, k| {
                let e = &entries[i];
                match k {
                    0 => e.label.clone(),
                    1 => e.date.clone(),
                    _ => e.notes.clone(),
                }
            })
        });
        self.exports.push(job.clone());
        self.show_exports = true;
        Some(job)
    }

    /// The feature's centre as longitude and latitude (WGS84), from its anchor on the active layer's grid.
    fn interp_lon_lat(&mut self, li: usize, i: usize) -> Option<(f64, f64)> {
        let a = *self.vectors.get(li)?.anchor.get(i)?;
        let g = self.session.as_ref()?.info.geo_transform?;
        if a[0].is_nan() {
            return None;
        }
        let (x, y) = (g[0] + g[1] * a[0] + g[2] * a[1], g[3] + g[4] * a[0] + g[5] * a[1]);
        self.status_lon_lat((x, y))
    }

    /// The cube's CRS and pixel size as the Point card gives them ("EPSG:32722 · 30 m pixel").
    fn interp_grid_text(&self) -> String {
        let Some(s) = &self.session else { return String::new() };
        let i = &s.info;
        let crs = if i.crs_authority.is_empty() { i.crs_name.clone() } else { i.crs_authority.clone() };
        let size = i.geo_transform.map_or(String::new(), |g| {
            let p = g[1].abs();
            if p < 0.1 { format!(" · {p:.5}° pixel") } else { format!(" · {} m pixel", fmt_size(p)) }
        });
        format!("{crs}{size}")
    }

    /// The Interpretation panel, as cards: the navigation, the point, the validation (its label, confidence, date,
    /// events and notes), the label set, the keys and the features' table.
    pub(crate) fn interpretation_ui(&mut self, ui: &mut Ui) {
        let Some(li) = self.interp_layer() else {
            ui.label(
                "Label the features of a vector layer one by one: the map goes to each, its series is drawn on the \
                 chart, a key gives its label.",
            );
            ui.add_space(4.0);
            if self.vectors.is_empty() {
                ui.weak("Add a vector layer first (Layers > Add vector layer...).");
            }
            let mut start = None;
            for v in &self.vectors {
                ui.horizontal(|ui| {
                    if ui.button("Start").clicked() {
                        start = Some(v.id);
                    }
                    ui.colored_label(v.color, &v.name);
                    ui.weak(format!("({})", v.data.kind_text()));
                });
            }
            if let Some(id) = start {
                self.start_interpretation(id);
            }
            return;
        };
        self.labels.sync_colors();
        let look = Look::new(ui);
        let (name, color, d) = {
            let v = &self.vectors[li];
            (v.name.clone(), v.color, v.data.clone())
        };
        let n = d.features.len();
        let labelled = self.labels.entries.iter().filter(|e| !e.label.is_empty()).count();
        let mut stop = false;
        ui.horizontal(|ui| {
            ui.colored_label(color, &name);
            stop = ui
                .small_button("Stop")
                .on_hover_text("End this session (its labels stay saved: starting again resumes it)")
                .clicked();
        });
        if stop {
            self.stop_interpretation();
            return;
        }
        // the navigation: previous, the point (a list of all), next, the next without a label
        let rows: Vec<usize> =
            if self.labels.filter.is_empty() { (0..n).collect() } else { self.labels.filter.clone() };
        let current = self.labels.current.filter(|&c| c < n);
        let (mut go, mut step, mut open) = (None, 0, false);
        ui.horizontal(|ui| {
            if ui.button("‹").on_hover_text("Previous point (Left, Shift+N, Page Up)").clicked() {
                step = -1;
            }
            let text = match current {
                Some(c) => format!(
                    "{} / {n} · id {}{}",
                    c + 1,
                    d.features[c].fid,
                    if self.labels.entries[c].label.is_empty() { "" } else { " ✓" }
                ),
                None => "-".to_string(),
            };
            let w = (ui.available_width() - 150.0).max(120.0);
            egui::ComboBox::from_id_salt("interp jump").width(w).selected_text(text).show_ui(ui, |ui| {
                let row_h = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
                egui::ScrollArea::vertical().max_height(280.0).show_rows(ui, row_h, rows.len(), |ui, range| {
                    for &i in &rows[range] {
                        let done = self.labels.entries.get(i).is_some_and(|e| !e.label.is_empty());
                        let t = format!("{} · id {}{}", i + 1, d.features[i].fid, if done { " ✓" } else { "" });
                        if ui.selectable_label(current == Some(i), t).clicked() {
                            go = Some(i);
                            ui.close();
                        }
                    }
                });
            });
            if ui.button("›").on_hover_text("Next point (Right, N, Page Down)").clicked() {
                step = 1;
            }
            open = ui
                .button("next open »")
                .on_hover_text("The next point without a label (O), round the end of the list")
                .clicked();
        });
        // the progress: thin and green, its words above
        ui.weak(format!("{labelled} / {n} validated"));
        ui.add(
            egui::ProgressBar::new(if n > 0 { labelled as f32 / n as f32 } else { 0.0 })
                .fill(look.ok)
                .desired_height(6.0)
                .corner_radius(3),
        );
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut self.labels.skip_labelled, "Skip labelled");
            ui.checkbox(&mut self.labels.advance, "Next after a label");
        });
        egui::CollapsingHeader::new("Zoom on each point").id_salt("interp zoom").show(ui, |ui| {
            ui.add(
                egui::Slider::new(&mut self.labels.zoom, 1.0..=32.0)
                    .logarithmic(true)
                    .fixed_decimals(0)
                    .text("points per pixel on each"),
            );
        });
        if let Some(i) = go {
            self.interp_go(i);
        } else if step != 0 {
            self.interp_step(step);
        } else if open {
            self.interp_next_open();
        }
        if self.labels.project {
            self.interpretation_project_ui(ui);
        }
        if let Some(c) = self.labels.current.filter(|&c| c < n) {
            ui.add_space(4.0);
            self.point_card(ui, &look, li, c, n);
            ui.add_space(6.0);
            self.validation_card(ui, &look, c);
            ui.add_space(6.0);
            if !d.fields.is_empty() {
                // read alone (attributes.rs), unless the layer's table is
                let values = self.vectors.get(li).and_then(|v| v.values(c)).map(<[String]>::to_vec);
                egui::CollapsingHeader::new("Attributes").show(ui, |ui| match values {
                    None => {
                        ui.weak("reading its fields...");
                    }
                    Some(values) => {
                        egui::Grid::new("interp attrs").num_columns(2).striped(true).show(ui, |ui| {
                            for (f, v) in d.fields.iter().zip(&values) {
                                ui.weak(f);
                                ui.label(v);
                                ui.end_row();
                            }
                        });
                    }
                });
            }
        }
        self.label_set_ui(ui);
        ui.add_space(4.0);
        card(ui, &look, |ui| {
            card_title(ui, &look, "Keys", "");
            keys_card(ui, &look);
        });
        ui.add_space(4.0);
        self.points_table_ui(ui, &d, &rows);
    }

    /// The Point card: its place in the list, its state, where it is, links to look at it elsewhere and, when the
    /// map may be seen, the map's class there.
    fn point_card(&mut self, ui: &mut Ui, look: &Look, li: usize, c: usize, n: usize) {
        let fid = self.vectors[li].data.features[c].fid;
        let label = self.labels.entries[c].label.clone();
        let ll = self.interp_lon_lat(li, c);
        let grid = self.interp_grid_text();
        // blind until labelled: the map's class only once the point has its label (or the blind is off)
        let seen = self.labels.project && (!self.val.blind || (self.val.reveal && !label.is_empty()));
        let reveal = if seen { self.validation_map_class(c, &label) } else { None };
        card(ui, look, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Point {} of {n}", c + 1)).small().color(look.muted));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if label.is_empty() {
                        badge(ui, "open", look.muted);
                    } else {
                        badge(ui, "validated", look.ok);
                    }
                });
            });
            ui.label(RichText::new(fid.to_string()).monospace().size(22.0).strong());
            ui.add_space(2.0);
            let coords = match ll {
                Some((lon, lat)) => format!("{lat:.5}, {lon:.5}"),
                None => "-".to_string(),
            };
            value_row(ui, look, "Lat, Lon", &coords);
            value_row(ui, look, "Grid", &grid);
            if let Some((name, agrees)) = &reveal {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Map").color(look.muted));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        match agrees {
                            Some(true) => ui.label(RichText::new("agrees").color(look.ok)),
                            Some(false) => ui.label(RichText::new("differs").color(look.bad)),
                            None => ui.label(""),
                        };
                        ui.label(RichText::new(format!("{name} ·")).monospace());
                    });
                });
            }
            if let Some((lon, lat)) = ll {
                ui.add_space(2.0);
                ui.horizontal_wrapped(|ui| {
                    let z = 18;
                    let links = [
                        (
                            "Google Maps ↗",
                            format!("https://www.google.com/maps/@{lat},{lon},{z}z/data=!3m1!1e3"),
                            look.link,
                        ),
                        (
                            "Esri Wayback ↗",
                            format!("https://livingatlas.arcgis.com/wayback/#mapCenter={lon}%2C{lat}%2C{z}"),
                            look.wayback,
                        ),
                        ("Google Earth ↗", format!("https://earth.google.com/web/@{lat},{lon},300d"), look.gold),
                    ];
                    for (text, url, c) in links {
                        ui.hyperlink_to(RichText::new(text).color(c), url);
                    }
                });
            }
        });
    }

    /// The Validation card: the label as buttons (colour, name, key), confidence, date of the change, other events
    /// and notes.
    fn validation_card(&mut self, ui: &mut Ui, look: &Look, c: usize) {
        let n = self.labels.entries.len();
        card(ui, look, |ui| {
            let saved = self.labels.saved.map_or(String::new(), |t| saved_ago(t.elapsed().as_secs()));
            card_title(ui, look, "Validation", &saved);
            let sp = ui.spacing().item_spacing.x;
            let half = ((ui.available_width() - sp) / 2.0).floor();
            let mut pick = None;
            let classes = self.labels.classes.clone();
            for row in classes.chunks(2).enumerate() {
                ui.horizontal(|ui| {
                    for (j, class) in row.1.iter().enumerate() {
                        let k = row.0 * 2 + j;
                        let on = self.labels.entries[c].label == *class;
                        let key = if k < 9 { (k + 1).to_string() } else { String::new() };
                        let swatch = self.labels.color32(k);
                        if key_button(ui, look, half, on, Some(swatch), class, &key).clicked() {
                            pick = Some(if on { None } else { Some(k) });
                        }
                    }
                });
            }
            if let Some(k) = pick {
                self.interp_label(k);
            }
            let Some(c) = self.labels.current.filter(|&c| c < n) else { return };
            ui.add_space(4.0);
            ui.label(RichText::new("Confidence").small().color(look.muted));
            let third = ((ui.available_width() - 2.0 * sp) / 3.0).floor();
            ui.horizontal(|ui| {
                for (k, (name, key)) in [("low", "Q"), ("medium", "W"), ("high", "E")].iter().enumerate() {
                    let v = k as i32 + 1;
                    let on = self.labels.entries[c].confidence == v;
                    if key_button(ui, look, third, on, None, name, key)
                        .on_hover_text(format!("{key} or Shift+{v}"))
                        .clicked()
                    {
                        self.interp_set_confidence(if on { 0 } else { v });
                    }
                }
            });
            ui.add_space(4.0);
            ui.label(RichText::new("Date of the change").small().color(look.muted));
            ui.horizontal(|ui| {
                let date = &self.labels.entries[c].date;
                ui.label(RichText::new(if date.is_empty() { "(none)" } else { date.as_str() }).monospace());
                if ui
                    .small_button("Map's date (D)")
                    .on_hover_text("The date shown on the map: go to it with [ and ] or a click on the chart")
                    .clicked()
                {
                    self.interp_set_date(false);
                }
                if ui.small_button("Clear (Shift+D)").clicked() {
                    self.interp_set_date(true);
                }
            });
            // more events at the feature (e.g. degraded, then cleared): a label and a date each
            let events = self.labels.entries[c].events.clone();
            egui::CollapsingHeader::new(format!("Other events ({})", events.len()))
                .id_salt("events")
                .default_open(!events.is_empty())
                .show(ui, |ui| {
                    let mut remove = None;
                    for (k, e) in events.iter().enumerate() {
                        ui.horizontal(|ui| {
                            if ui.small_button("x").clicked() {
                                remove = Some(k);
                            }
                            ui.label(format!("{}  {}", e.date, e.label));
                        });
                    }
                    if let Some(k) = remove {
                        self.labels.entries[c].events.remove(k);
                        self.interp_touched();
                    }
                    ui.horizontal(|ui| {
                        let l = &mut self.labels;
                        let shown =
                            if l.event_label.is_empty() { "(the label)".to_string() } else { l.event_label.clone() };
                        egui::ComboBox::from_id_salt("event label")
                            .width(ui.available_width() * 0.5)
                            .selected_text(shown)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut l.event_label, String::new(), "(the label)");
                                for class in &l.classes {
                                    ui.selectable_value(&mut l.event_label, class.clone(), class);
                                }
                            });
                        if ui.small_button("Add at the map's date (Shift+E)").clicked() {
                            let l = if self.labels.event_label.is_empty() {
                                self.labels.entries[c].label.clone()
                            } else {
                                self.labels.event_label.clone()
                            };
                            self.interp_add_event(l);
                        }
                    });
                });
            ui.label(RichText::new("Notes").small().color(look.muted));
            let row = ui.text_style_height(&egui::TextStyle::Body);
            if ui
                .add(
                    egui::TextEdit::multiline(&mut self.labels.notes)
                        .desired_width(f32::INFINITY)
                        .min_size(vec2(0.0, row * 3.0)),
                )
                .changed()
            {
                self.labels.entries[c].notes = self.labels.notes.clone();
                self.interp_touched();
            }
        });
    }

    /// The label set: names and colours, added to or removed from.
    fn label_set_ui(&mut self, ui: &mut Ui) {
        let mut set_changed = false;
        egui::CollapsingHeader::new("Label set").show(ui, |ui| {
            let l = &mut self.labels;
            let mut remove = None;
            let mut changed = false;
            for (k, class) in l.classes.iter().enumerate() {
                ui.horizontal(|ui| {
                    if ui.small_button("x").clicked() {
                        remove = Some(k);
                    }
                    if let Some(c) = l.colors.get_mut(k)
                        && ui.color_edit_button_srgb(c).on_hover_text("The label's colour").changed()
                    {
                        changed = true;
                    }
                    ui.label(if k < 9 { format!("{}  {class}", k + 1) } else { format!("   {class}") });
                });
            }
            if let Some(k) = remove {
                l.classes.remove(k);
                if k < l.colors.len() {
                    l.colors.remove(k);
                }
                changed = true;
            }
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut l.new_class).desired_width(ui.available_width() - 60.0));
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.button("Add").clicked() || enter) && !l.new_class.is_empty() && !l.classes.contains(&l.new_class)
                {
                    // the next colour of the categorical palette
                    let k = l.classes.len();
                    let name = std::mem::take(&mut l.new_class);
                    l.colors.push(default_class_color(&name, k));
                    l.classes.push(name);
                    changed = true;
                }
            });
            if changed && l.project {
                set_changed = true;
            } else if changed {
                l.dirty = true;
                l.changed_at = Some(Instant::now());
            }
            ui.weak("Labels already given keep their text when the set changes.");
        });
        if set_changed {
            self.validation_label_set_changed();
        }
    }

    /// "All points": the table of the features (or the filter's) with the export of the labels.
    fn points_table_ui(&mut self, ui: &mut Ui, d: &Arc<janus_geo::vectors::VectorData>, rows: &[usize]) {
        let title = if self.labels.filter.is_empty() {
            "All points".to_string()
        } else {
            format!("All points: {}", self.labels.filter_name)
        };
        let mut go = None;
        egui::CollapsingHeader::new(title).id_salt("interp all points").show(ui, |ui| {
            ui.horizontal(|ui| {
                if !self.labels.filter.is_empty() && ui.small_button("Show all").clicked() {
                    self.labels.filter.clear();
                    self.labels.filter_name.clear();
                }
                if self.labels.project {
                    ui.weak("saved in the project as you go").on_hover_text(&d.path);
                    return;
                }
                if ui
                    .button("Export table...")
                    .on_hover_text(
                        "The file's features and fields with label, change_date and notes:\n\
                         GeoPackage, GeoJSON or Shapefile (with the geometries), or CSV (with lon/lat)",
                    )
                    .clicked()
                {
                    let stem = std::path::Path::new(&d.path)
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let mut dlg = rfd::FileDialog::new()
                        .set_title("Export the labels (.csv, .gpkg, .geojson or .shp)")
                        .set_file_name(format!("{stem}_labels.gpkg"))
                        .add_filter("GeoPackage", &["gpkg"])
                        .add_filter("CSV", &["csv"])
                        .add_filter("GeoJSON", &["geojson"])
                        .add_filter("Shapefile", &["shp"]);
                    let folder = self.export_folder();
                    if !folder.is_empty() {
                        dlg = dlg.set_directory(folder);
                    }
                    if let Some(p) = dlg.save_file() {
                        let p = p.to_string_lossy().into_owned();
                        self.remember_export_folder(&p);
                        self.export_labels(p, ui.ctx());
                    }
                }
                let file = self.labels_file().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
                ui.weak("saved as you go").on_hover_text(file);
            });
            // the rows: every feature, or the filter's
            let row_h = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
            let current = self.labels.current;
            let scroll = std::mem::take(&mut self.labels.scroll_to_current);
            let height = 320.0;
            let mut area =
                egui::ScrollArea::vertical().id_salt("interp rows").max_height(height).auto_shrink([false, true]);
            // show_rows lays out only the rows in view: a current row out of it is brought to the middle by the offset
            if let Some(k) = current.filter(|_| scroll).and_then(|c| rows.iter().position(|&i| i == c)) {
                let pitch = row_h + ui.spacing().item_spacing.y;
                area = area.vertical_scroll_offset((k as f32 * pitch - (height - pitch) * 0.5).max(0.0));
            }
            area.show_rows(ui, row_h, rows.len(), |ui, range| {
                egui::Grid::new("interp rows grid").num_columns(5).striped(true).show(ui, |ui| {
                    for &i in &rows[range] {
                        let Some(e) = self.labels.entries.get(i) else { continue };
                        let r = ui.selectable_label(current == Some(i), (i + 1).to_string());
                        if r.clicked() {
                            go = Some(i);
                        }
                        let text = if e.events.is_empty() {
                            e.label.clone()
                        } else {
                            format!("{} +{}", e.label, e.events.len())
                        };
                        match self.labels.class_color32(&e.label) {
                            Some(c) => ui.label(RichText::new(format!("■ {text}")).color(c)),
                            None => ui.label(text),
                        };
                        ui.label(&e.date);
                        ui.label(match e.confidence {
                            1 => "low",
                            2 => "med.",
                            3 => "high",
                            _ => "",
                        });
                        ui.label(&e.notes);
                        ui.end_row();
                    }
                });
            });
        });
        if let Some(i) = go {
            self.interp_go(i);
        }
    }
}

impl Interpretation {
    /// One colour per class: those missing take the default of their name.
    fn sync_colors(&mut self) {
        self.colors.truncate(self.classes.len());
        while self.colors.len() < self.classes.len() {
            let k = self.colors.len();
            self.colors.push(default_class_color(&self.classes[k], k));
        }
    }

    /// The colour of class k.
    pub fn color32(&self, k: usize) -> Color32 {
        let c = self.colors.get(k).copied().unwrap_or([128, 128, 128]);
        Color32::from_rgb(c[0], c[1], c[2])
    }

    /// The colour of the class named `label` (None: not of the set).
    pub fn class_color32(&self, label: &str) -> Option<Color32> {
        self.classes.iter().position(|c| c == label).map(|k| self.color32(k))
    }
}

/// A size of a pixel without trailing zeros: 30, 0.5, 12.25.
fn fmt_size(p: f64) -> String {
    let s = format!("{p:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// The colours of the cards, from the theme (the light and the dark ones).
struct Look {
    muted: Color32,
    gold: Color32,
    ok: Color32,
    bad: Color32,
    link: Color32,
    wayback: Color32,
    fill: Color32,
    border: Color32,
    chip_fill: Color32,
    text: Color32,
    selected_fill: Color32,
    hover_fill: Color32,
}

impl Look {
    fn new(ui: &Ui) -> Look {
        let v = ui.visuals();
        let dark = v.dark_mode;
        let pick = |d: [u8; 3], l: [u8; 3]| {
            let c = if dark { d } else { l };
            Color32::from_rgb(c[0], c[1], c[2])
        };
        Look {
            muted: v.weak_text_color(),
            gold: pick([0xf2, 0xc9, 0x4c], [0x9a, 0x74, 0x00]),
            ok: pick([0x5f, 0xcf, 0x8a], [0x1e, 0x86, 0x4a]),
            bad: pick([0xef, 0x6b, 0x6b], [0xc0, 0x39, 0x2b]),
            link: pick([0x6e, 0xa8, 0xfe], [0x1a, 0x5f, 0xd0]),
            wayback: pick([0xf0, 0xa0, 0x4b], [0xb8, 0x62, 0x00]),
            fill: v.faint_bg_color,
            border: v.widgets.noninteractive.bg_stroke.color,
            chip_fill: v.widgets.inactive.weak_bg_fill,
            text: v.text_color(),
            selected_fill: v.selection.bg_fill,
            hover_fill: v.widgets.hovered.weak_bg_fill,
        }
    }
}

/// A card: a rounded frame as wide as the panel.
fn card<R>(ui: &mut Ui, look: &Look, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .fill(look.fill)
        .stroke(Stroke::new(1.0, look.border))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// A card's title in small capitals, with a weak note at its right.
fn card_title(ui: &mut Ui, look: &Look, title: &str, note: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(title.to_uppercase()).small().strong().color(look.muted));
        if !note.is_empty() {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(note).small().color(look.muted));
            });
        }
    });
}

/// A weak name at the left, its value in monospace at the right.
fn value_row(ui: &mut Ui, look: &Look, name: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(name).color(look.muted));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(value).monospace());
        });
    });
}

/// A state tag with a border: "open" grey, "validated" green.
fn badge(ui: &mut Ui, text: &str, color: Color32) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.14))
        .stroke(Stroke::new(1.0, color))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(8, 1))
        .show(ui, |ui| {
            ui.label(RichText::new(text).small().color(color));
        });
}

/// A key as a small bordered chip, inline.
fn kbd(ui: &mut Ui, look: &Look, text: &str) {
    egui::Frame::new()
        .fill(look.chip_fill)
        .stroke(Stroke::new(1.0, look.border))
        .corner_radius(CornerRadius::same(3))
        .inner_margin(Margin::symmetric(4, 0))
        .show(ui, |ui| {
            ui.label(RichText::new(text).monospace().small().color(look.text));
        });
}

/// A button `width` wide with an optional square of the label's colour, its name and its key as a chip at the
/// right; `selected`: tinted with the colour and outlined by it.
fn key_button(
    ui: &mut Ui,
    look: &Look,
    width: f32,
    selected: bool,
    swatch: Option<Color32>,
    text: &str,
    key: &str,
) -> egui::Response {
    let body = egui::TextStyle::Body.resolve(ui.style());
    let height = ui.text_style_height(&egui::TextStyle::Body) + 10.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    let fill = if selected {
        swatch.map_or(look.selected_fill, |c| c.gamma_multiply(0.22))
    } else if resp.hovered() {
        look.hover_fill
    } else {
        look.chip_fill
    };
    let stroke = if selected { Stroke::new(1.5, swatch.unwrap_or(look.gold)) } else { Stroke::new(1.0, look.border) };
    let p = ui.painter();
    p.rect(rect, CornerRadius::same(6), fill, stroke, egui::StrokeKind::Inside);
    let mut left = rect.left() + 8.0;
    if let Some(c) = swatch {
        let r = Rect::from_center_size(pos2(left + 6.0, rect.center().y), vec2(12.0, 12.0));
        p.rect_filled(r, CornerRadius::same(3), c);
        left += 20.0;
    }
    let mut right = rect.right() - 8.0;
    if !key.is_empty() {
        let g = p.layout_no_wrap(key.to_string(), egui::FontId::monospace(body.size * 0.85), look.muted);
        let (w, h) = (g.size().x + 8.0, g.size().y + 2.0);
        let r = Rect::from_min_size(pos2(right - w, rect.center().y - h / 2.0), vec2(w, h));
        p.rect(r, CornerRadius::same(3), look.fill, Stroke::new(1.0, look.border), egui::StrokeKind::Inside);
        p.galley(r.min + vec2(4.0, 1.0), g, look.muted);
        right = r.left() - 6.0;
    }
    p.with_clip_rect(Rect::from_min_max(pos2(left, rect.top()), pos2(right.max(left), rect.bottom()))).text(
        pos2(left, rect.center().y),
        egui::Align2::LEFT_CENTER,
        text,
        body,
        look.text,
    );
    resp
}

/// The Keys card: each line a run of chips (keys) and words.
fn keys_card(ui: &mut Ui, look: &Look) {
    // (true: a key chip, false: words)
    let lines: [&[(bool, &str)]; 6] = [
        &[
            (true, "←"),
            (true, "→"),
            (false, "point"),
            (true, "N"),
            (false, "next"),
            (true, "Shift+N"),
            (false, "previous"),
        ],
        &[(true, "O"), (false, "next open"), (true, "1-9"), (false, "label"), (true, "0"), (false, "clears it")],
        &[(true, "Q"), (true, "W"), (true, "E"), (false, "confidence (or"), (true, "Shift+1-3"), (false, ")")],
        &[(true, "D"), (false, "date of the change"), (true, "Shift+D"), (false, "clears it")],
        &[(true, "Shift+E"), (false, "other event at the map's date")],
        &[(true, "["), (true, "]"), (false, "the map's date (or click the chart)")],
    ];
    for line in lines {
        ui.horizontal_wrapped(|ui| {
            for &(chip, t) in line {
                if chip {
                    kbd(ui, look, t);
                } else {
                    ui.label(RichText::new(t).color(look.muted));
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_of_the_label_set() {
        // the nine defaults have theirs, a new label the next of the categorical palette
        assert_eq!(default_class_color("Stable forest", 0), DEFAULT_COLORS[0]);
        assert_eq!(default_class_color("cloud / no data", 7), DEFAULT_COLORS[7]);
        assert_eq!(default_class_color("Wetland", 9), EXTRA_COLORS[1]);
        assert_ne!(default_class_color("Wetland", 9), default_class_color("Mangrove", 10));
        // stored as text, read back; damaged or missing ones (an old project) take the defaults
        let classes: Vec<String> = ["Water", "Other", "Wetland"].map(String::from).to_vec();
        let hex = vec![color_hex([1, 2, 255]), "nonsense".to_string()];
        let c = colors_from_hex(&classes, &hex);
        assert_eq!(c, vec![[1, 2, 255], DEFAULT_COLORS[8], EXTRA_COLORS[2]]);
        assert_eq!(colors_from_hex(&classes, &[]), vec![DEFAULT_COLORS[6], DEFAULT_COLORS[8], EXTRA_COLORS[2]]);
        assert_eq!(parse_color_hex("#0a0B0c"), Some([10, 11, 12]));
        assert_eq!(parse_color_hex("0a0b0c"), None);
        assert_eq!(parse_color_hex("#12"), None);
    }
}

/// "saved just now", "saved 12 s ago", "saved 3 min ago": how long ago the labels were written.
fn saved_ago(s: u64) -> String {
    match s {
        0..=2 => "saved just now".into(),
        3..=59 => format!("saved {s} s ago"),
        60..=3599 => format!("saved {} min ago", s / 60),
        _ => format!("saved {} h ago", s / 3600),
    }
}
