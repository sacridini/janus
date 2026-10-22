//! Interpretation (src/app_labels.cpp): a vector layer's features gone through one by one, as TimeSync does with
//! Landsat: the map centred on each (zoomed in), its series on the chart (a pin at a point; a polygon becomes the
//! ROI), a label from the label set (keys 1-9), the date of the change (D: the date on the map) and notes, its
//! confidence and more events. Labels are saved as they are given (in the app data, by file and layer: opening
//! the same file again resumes the work) and exported as a table (CSV, GeoPackage, GeoJSON, Shapefile) with the
//! file's own fields. (A validation project's sample, phase 8, saves them in its GeoPackage instead.)

use std::sync::Arc;
use std::time::Instant;

use egui::{Ui, vec2};
use janus_geo::vectors::{Kind, write_vector_table};
use serde::{Deserialize, Serialize};
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

/// Another event at a feature (e.g. degraded, then cleared).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Event {
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub date: String,
}

/// A feature's label (the C++'s val::Label).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Label {
    /// a class of the label set ("" = not labelled)
    #[serde(default)]
    pub label: String,
    /// the date of the change, as the series shows it ("" = none)
    #[serde(default)]
    pub date: String,
    #[serde(default)]
    pub notes: String,
    /// 0 not given, 1 low, 2 medium, 3 high
    #[serde(default)]
    pub confidence: i32,
    #[serde(default)]
    pub events: Vec<Event>,
    /// UTC, ISO 8601
    #[serde(default)]
    pub updated: String,
}

impl Label {
    pub fn is_empty(&self) -> bool {
        self.label.is_empty()
            && self.date.is_empty()
            && self.notes.is_empty()
            && self.confidence == 0
            && self.events.is_empty()
    }
}

/// Now in UTC, ISO 8601.
pub fn now_iso() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (y, m, d) = janus_zeit::numeric::civil(now.div_euclid(86400));
    let s = now.rem_euclid(86400);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", s / 3600, s / 60 % 60, s % 60)
}

/// The interpretation going on.
pub struct Interpretation {
    /// the vector layer's id (None: none)
    pub vector: Option<u32>,
    /// the feature shown
    pub current: Option<usize>,
    /// the label set (keys 1-9)
    pub classes: Vec<String>,
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
}

impl Default for Interpretation {
    fn default() -> Self {
        Interpretation {
            vector: None,
            current: None,
            classes: Vec::new(),
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
        l.entries = vec![Label::default(); n];
        self.load_labels();
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

    /// The next (+1) or previous (-1) feature with a geometry (unlabelled ones only with "Skip labelled").
    pub fn interp_step(&mut self, dir: i64) {
        let Some(li) = self.interp_layer() else { return };
        let d = self.vectors[li].data.clone();
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

    fn interp_set_confidence(&mut self, v: i32) {
        let Some(c) = self.labels.current.filter(|&c| c < self.labels.entries.len()) else { return };
        self.labels.entries[c].confidence = v.clamp(0, 3);
        self.interp_touched();
    }

    fn interp_add_event(&mut self, label: String) {
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
    }

    /// N / Shift+N (Page Down / Up), 1-9 a label, 0 clears it, D / Shift+D the date, Shift+1-3 the confidence,
    /// E an event at the map's date.
    pub(crate) fn interp_shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers};
        if !self.labels.show || self.labels.vector.is_none() {
            return;
        }
        let chord = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_key(m, k));
        if chord(Modifiers::NONE, Key::N) || chord(Modifiers::NONE, Key::PageDown) {
            self.interp_step(1);
        }
        if chord(Modifiers::SHIFT, Key::N) || chord(Modifiers::NONE, Key::PageUp) {
            self.interp_step(-1);
        }
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
        for (k, &key) in DIGITS.iter().enumerate().skip(1) {
            if chord(Modifiers::NONE, key) {
                self.interp_label(Some(k - 1));
            }
        }
        if chord(Modifiers::NONE, Key::Num0) {
            self.interp_label(None);
        }
        if chord(Modifiers::NONE, Key::D) {
            self.interp_set_date(false);
        }
        if chord(Modifiers::SHIFT, Key::D) {
            self.interp_set_date(true);
        }
        for (c, &key) in DIGITS.iter().enumerate().take(4).skip(1) {
            // low, medium, high confidence
            if chord(Modifiers::SHIFT, key) {
                self.interp_set_confidence(c as i32);
            }
        }
        if chord(Modifiers::NONE, Key::E)
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
        let (Some(li), Some(path)) = (self.interp_layer(), self.labels_file()) else { return };
        let d = &self.vectors[li].data;
        let entries: Vec<Value> = self
            .labels
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| !e.is_empty())
            .map(|(i, e)| {
                let mut o = serde_json::to_value(e).unwrap_or_default();
                o["fid"] = json!(d.features[i].fid);
                o
            })
            .collect();
        let j = json!({"path": d.path, "layer": d.layer, "classes": self.labels.classes, "entries": entries});
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
        }
        let d = &self.vectors[li].data;
        let by_fid: std::collections::HashMap<i64, usize> =
            d.features.iter().enumerate().map(|(i, f)| (f.fid, i)).collect();
        for e in j["entries"].as_array().into_iter().flatten() {
            if let Some(&i) = by_fid.get(&e["fid"].as_i64().unwrap_or(-1)) {
                self.labels.entries[i] = serde_json::from_value(e.clone()).unwrap_or_default();
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

    /// The Interpretation panel: the feature shown, its label, date, confidence, events and notes, the label set,
    /// the features' table.
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
        ui.add(
            egui::ProgressBar::new(if n > 0 { labelled as f32 / n as f32 } else { 0.0 })
                .text(format!("{labelled} of {n} labelled")),
        );
        ui.horizontal(|ui| {
            if ui.small_button("<").on_hover_text("Previous (Shift+N, Page Up)").clicked() {
                self.interp_step(-1);
            }
            if ui.small_button(">").on_hover_text("Next (N, Page Down)").clicked() {
                self.interp_step(1);
            }
            if let Some(c) = self.labels.current {
                ui.label(format!("{} / {n}   (fid {})", c + 1, d.features[c].fid));
            }
        });
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.labels.skip_labelled, "Skip labelled");
            ui.checkbox(&mut self.labels.advance, "Next after a label");
        });
        ui.add(
            egui::Slider::new(&mut self.labels.zoom, 1.0..=32.0)
                .logarithmic(true)
                .fixed_decimals(0)
                .text("points per pixel on each"),
        );
        if let Some(c) = self.labels.current.filter(|&c| c < n) {
            crate::panels::section(ui, "Label (keys 1-9, 0 clears)");
            let bw = (ui.available_width() - ui.spacing().item_spacing.x * 2.0) / 3.0;
            let mut pick = None;
            let classes = self.labels.classes.clone();
            for row in classes.chunks(3).enumerate() {
                ui.horizontal(|ui| {
                    for (j, class) in row.1.iter().enumerate() {
                        let k = row.0 * 3 + j;
                        let on = self.labels.entries[c].label == *class;
                        let text = if k < 9 { format!("{}  {class}", k + 1) } else { class.clone() };
                        if ui.add(egui::Button::selectable(on, text).min_size(vec2(bw, 0.0))).clicked() {
                            pick = Some(if on { None } else { Some(k) });
                        }
                    }
                });
            }
            if let Some(k) = pick {
                self.interp_label(k);
            }
            let Some(c) = self.labels.current.filter(|&c| c < n) else { return };
            crate::panels::section(ui, "Date of the change");
            ui.horizontal(|ui| {
                let date = &self.labels.entries[c].date;
                ui.label(if date.is_empty() { "(none)" } else { date.as_str() });
                if ui
                    .small_button("Map's date (D)")
                    .on_hover_text("The date shown on the map: go to it with the arrows or a click on the chart")
                    .clicked()
                {
                    self.interp_set_date(false);
                }
                if ui.small_button("Clear (Shift+D)").clicked() {
                    self.interp_set_date(true);
                }
            });
            // confidence: how sure the interpreter is (kept with the label)
            ui.horizontal(|ui| {
                ui.label("Confidence");
                for (k, name) in ["low", "medium", "high"].iter().enumerate() {
                    let v = k as i32 + 1;
                    let on = self.labels.entries[c].confidence == v;
                    if ui.add(egui::Button::selectable(on, *name)).on_hover_text(format!("Shift+{v}")).clicked() {
                        self.interp_set_confidence(if on { 0 } else { v });
                    }
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
                        if ui.small_button("Add at the map's date (E)").clicked() {
                            let l = if self.labels.event_label.is_empty() {
                                self.labels.entries[c].label.clone()
                            } else {
                                self.labels.event_label.clone()
                            };
                            self.interp_add_event(l);
                        }
                    });
                });
            crate::panels::section(ui, "Notes");
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
            if !d.fields.is_empty() {
                egui::CollapsingHeader::new("Attributes").show(ui, |ui| {
                    egui::Grid::new("interp attrs").num_columns(2).striped(true).show(ui, |ui| {
                        for (f, v) in d.fields.iter().zip(&d.features[c].values) {
                            ui.weak(f);
                            ui.label(v);
                            ui.end_row();
                        }
                    });
                });
            }
        }
        egui::CollapsingHeader::new("Label set").show(ui, |ui| {
            let l = &mut self.labels;
            let mut remove = None;
            for (k, class) in l.classes.iter().enumerate() {
                ui.horizontal(|ui| {
                    if ui.small_button("x").clicked() {
                        remove = Some(k);
                    }
                    ui.label(if k < 9 { format!("{}  {class}", k + 1) } else { format!("   {class}") });
                });
            }
            let mut changed = false;
            if let Some(k) = remove {
                l.classes.remove(k);
                changed = true;
            }
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut l.new_class).desired_width(ui.available_width() - 60.0));
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.button("Add").clicked() || enter) && !l.new_class.is_empty() && !l.classes.contains(&l.new_class)
                {
                    l.classes.push(std::mem::take(&mut l.new_class));
                    changed = true;
                }
            });
            if changed {
                l.dirty = true;
                l.changed_at = Some(Instant::now());
            }
            ui.weak("Labels already given keep their text when the set changes.");
        });
        crate::panels::section(ui, "Features");
        ui.horizontal(|ui| {
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
        // the rows: every feature
        let row_h = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
        let current = self.labels.current;
        let scroll = std::mem::take(&mut self.labels.scroll_to_current);
        let mut go = None;
        egui::ScrollArea::vertical().id_salt("interp rows").auto_shrink(false).show_rows(ui, row_h, n, |ui, range| {
            egui::Grid::new("interp rows grid").num_columns(5).striped(true).show(ui, |ui| {
                for i in range {
                    let e = &self.labels.entries[i];
                    let r = ui.selectable_label(current == Some(i), (i + 1).to_string());
                    if r.clicked() {
                        go = Some(i);
                    }
                    if scroll && current == Some(i) {
                        r.scroll_to_me(Some(egui::Align::Center));
                    }
                    ui.label(if e.events.is_empty() {
                        e.label.clone()
                    } else {
                        format!("{} +{}", e.label, e.events.len())
                    });
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
        if let Some(i) = go {
            self.interp_go(i);
        }
    }
}
