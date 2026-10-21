//! Categorical series (land cover, masks, classifications...): detection, class colours and names, the GPU's table
//! of class colours and the legend (src/app_classes.cpp). A layer is categorical when its file says so (colour
//! table, category names, attribute table) or, failing that, when the first date read has only a few distinct whole
//! values. The Display panel can override the detection.

use std::collections::BTreeMap;

use egui::{Color32, Ui};
use janus_cube::CubeInfo;
use janus_gpu::{CLASS_LUT_SIZE, Gpu, GpuTexture};

use crate::session::Session;

/// More distinct values than this: continuous data.
const MAX_DETECTED: usize = 40;
/// The most a layer can show as classes.
const MAX_CLASSES: usize = 256;
/// Values counted once the overview is complete.
const MAX_SAMPLES: usize = 20_000_000;

/// Tableau 20: distinct colours for up to 20 classes without a colour table.
const PALETTE: [[u8; 3]; 20] = [
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
    [174, 199, 232],
    [255, 187, 120],
    [152, 223, 138],
    [255, 152, 150],
    [197, 176, 213],
    [196, 156, 148],
    [247, 182, 210],
    [199, 199, 199],
    [219, 219, 141],
    [158, 218, 229],
];

#[derive(Clone, Debug)]
pub struct ClassEntry {
    pub value: i32,
    pub name: String,
    pub color: [u8; 3],
    pub visible: bool,
    pub count: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ClassState {
    #[default]
    Undecided,
    Off,
    On,
}

#[derive(Default)]
pub struct LayerClasses {
    pub state: ClassState,
    /// counted on every date
    pub complete: bool,
    /// sorted by value
    pub list: Vec<ClassEntry>,
    pub lut: Option<GpuTexture>,
    lut_dirty: bool,
    error: String,
}

/// Counts of the whole values (every `stride`-th); None if a finite value is not whole or there are more than
/// `max` of them.
fn count_classes(data: &[f32], stride: usize, max: usize, counts: &mut BTreeMap<i32, usize>) -> Option<()> {
    for &v in data.iter().step_by(stride.max(1)) {
        if !v.is_finite() {
            continue;
        }
        let r = v.round();
        if r != v || r.abs() > 1e9 {
            return None;
        }
        *counts.entry(r as i32).or_default() += 1;
        if counts.len() > max {
            return None;
        }
    }
    Some(())
}

impl LayerClasses {
    pub fn on(&self) -> bool {
        self.state == ClassState::On
    }

    pub fn find(&self, value: i32) -> Option<&ClassEntry> {
        self.list.binary_search_by_key(&value, |c| c.value).ok().map(|i| &self.list[i])
    }

    /// A value's class as shown: "Forest (3)".
    pub fn name_of(&self, v: f32) -> String {
        if !v.is_finite() {
            return "-".into();
        }
        let k = v.round() as i32;
        self.find(k).map_or_else(|| k.to_string(), |c| format!("{} ({k})", c.name))
    }

    /// Rebuilds the class list from value counts, keeping the names and colours the user already edited.
    fn set(&mut self, info: &CubeInfo, counts: &BTreeMap<i32, usize>) {
        // palette colours already taken by classes that keep theirs: a class that appears later gets the next free one
        let mut used = [false; 20];
        let palette_index = |c: [u8; 3]| PALETTE.iter().position(|p| *p == c);
        for v in counts.keys() {
            if let Some(i) = self.find(*v).and_then(|old| palette_index(old.color)) {
                used[i] = true;
            }
        }
        let mut k = 0;
        let mut next_free = || {
            let mut tries = 0;
            while tries < 20 && used[k % 20] {
                k += 1;
                tries += 1;
            }
            used[k % 20] = true;
            k % 20
        };
        let list = counts
            .iter()
            .map(|(&value, &count)| match self.find(value) {
                Some(old) => ClassEntry { count, ..old.clone() },
                None => ClassEntry {
                    value,
                    name: info.class_names.get(&value).cloned().unwrap_or_else(|| format!("Class {value}")),
                    color: match info.class_colors.get(&value) {
                        Some(c) => [c[0], c[1], c[2]],
                        None => PALETTE[next_free()],
                    },
                    visible: true,
                    count,
                },
            })
            .collect();
        self.list = list;
        self.lut_dirty = true;
    }

    /// Counts the classes over every date read (subsampled to MAX_SAMPLES values).
    fn count_all(s: &Session, max: usize) -> Option<BTreeMap<i32, usize>> {
        let ov = &s.s.overview;
        let per = ov.w * ov.h;
        let loaded = s.uploaded.iter().filter(|&&u| u).count();
        if loaded == 0 || per == 0 {
            return None;
        }
        let stride = (per * loaded / MAX_SAMPLES).max(1);
        let mut counts = BTreeMap::new();
        for t in (0..ov.t).filter(|&t| s.uploaded[t]) {
            count_classes(ov.layer(t)?, stride, max, &mut counts)?;
        }
        Some(counts)
    }

    /// Per frame: decides whether the layer is categorical (Some(true) then: the map shows values), completes the
    /// class list once every date is read, refreshes the GPU's table. Returns whether the map changed.
    pub fn update(&mut self, s: &Session, gpu: &Gpu) -> (bool, bool) {
        let mut changed = false;
        let mut became_on = false;
        let info = &s.info;
        if self.state == ClassState::Undecided
            && let Some(first) = (0..info.t()).find(|&t| s.uploaded[t])
            && let Some(layer) = s.s.overview.layer(first)
        {
            let mut counts = BTreeMap::new();
            let max = if info.file_categorical { MAX_CLASSES } else { MAX_DETECTED };
            let few = count_classes(layer, 1, max, &mut counts).is_some();
            // declared by the file: classes even if a date has many values; reflectances never classes
            if (info.file_categorical || few) && !counts.is_empty() && !info.spectral {
                self.state = ClassState::On;
                self.set(info, &counts);
                became_on = true;
            } else {
                self.state = ClassState::Off;
            }
            changed = true;
        }
        if self.on() && !self.complete && s.s.overview.complete() {
            // every date read: the classes that appear later in the series too
            if let Some(counts) = Self::count_all(s, MAX_CLASSES) {
                self.set(info, &counts);
            }
            self.complete = true;
        }
        if self.lut_dirty && self.on() {
            let mut lut = vec![0u8; CLASS_LUT_SIZE as usize * 4];
            for c in self.list.iter().filter(|c| c.value >= 0 && (c.value as u32) < CLASS_LUT_SIZE) {
                let at = c.value as usize * 4;
                lut[at..at + 3].copy_from_slice(&c.color);
                lut[at + 3] = if c.visible { 255 } else { 0 };
            }
            self.lut = Some(gpu.create_class_lut(&lut));
            self.lut_dirty = false;
            changed = true;
        }
        (changed, became_on)
    }

    /// The Display panel's switch and legend (colours, visibility and names editable). Returns whether the map
    /// changed and whether classes were turned on (the map then shows values).
    pub fn ui(&mut self, ui: &mut Ui, s: &Session) -> (bool, bool) {
        if self.state == ClassState::Undecided {
            return (false, false);
        }
        let mut on = self.on();
        let mut changed = false;
        let mut turned_on = false;
        let r = ui.checkbox(&mut on, "Categorical (classes)").on_hover_text(
            "Each value is a class with its own colour (land cover, masks, classifications).\n\
             Detected from the file's colour table or category names, or from a few whole values.",
        );
        if r.changed() {
            if on {
                match Self::count_all(s, MAX_CLASSES).filter(|c| !c.is_empty()) {
                    Some(counts) => {
                        self.state = ClassState::On;
                        self.complete = s.s.overview.complete();
                        self.set(&s.info, &counts);
                        turned_on = true;
                    }
                    None => {
                        self.error = format!(
                            "Not categorical: more than {MAX_CLASSES} distinct values, or values that are not whole numbers."
                        )
                    }
                }
            } else {
                self.state = ClassState::Off;
            }
            changed = true;
        }
        if !self.error.is_empty() && !self.on() {
            ui.colored_label(Color32::from_rgb(230, 80, 70), &self.error);
        }
        if !self.on() {
            return (changed, turned_on);
        }
        self.error.clear();
        let total: usize = self.list.iter().map(|c| c.count).sum();
        ui.separator();
        ui.label("Classes");
        egui::ScrollArea::vertical().max_height(320.0).id_salt("classes").show(ui, |ui| {
            egui::Grid::new("classes grid").striped(true).num_columns(4).show(ui, |ui| {
                ui.weak("");
                ui.weak("Value");
                ui.weak("Name");
                ui.weak("Share");
                ui.end_row();
                for c in &mut self.list {
                    ui.horizontal(|ui| {
                        if ui.checkbox(&mut c.visible, "").on_hover_text("Show this class on the map").changed() {
                            self.lut_dirty = true;
                        }
                        if ui.color_edit_button_srgb(&mut c.color).changed() {
                            self.lut_dirty = true;
                        }
                    });
                    ui.label(c.value.to_string());
                    ui.add(egui::TextEdit::singleline(&mut c.name).desired_width(140.0));
                    ui.weak(format!("{:.1}%", if total > 0 { 100.0 * c.count as f64 / total as f64 } else { 0.0 }));
                    ui.end_row();
                }
            });
        });
        if !self.complete {
            ui.weak("Shares from the dates read so far.");
        }
        if self.list.last().is_some_and(|c| c.value >= CLASS_LUT_SIZE as i32) {
            ui.weak(format!("Values from {CLASS_LUT_SIZE} on are drawn grey."));
        }
        (changed || self.lut_dirty, turned_on)
    }

    /// Statistics of a categorical series: majority class, changes, last change.
    pub fn summary(&self, info: &CubeInfo, v: &[f32], t: usize) -> Vec<(String, String)> {
        let mut count: BTreeMap<i32, usize> = BTreeMap::new();
        let (mut changes, mut last, mut last_change, mut prev, mut n) =
            (0, None::<usize>, None::<usize>, None::<usize>, 0);
        for (i, &x) in v.iter().enumerate() {
            if !x.is_finite() {
                continue;
            }
            let k = x.round() as i32;
            *count.entry(k).or_default() += 1;
            n += 1;
            if let Some(l) = last
                && k != v[l].round() as i32
            {
                changes += 1;
                last_change = Some(i);
                prev = Some(l);
            }
            last = Some(i);
        }
        let mut rows = vec![
            ("Valid n".to_string(), n.to_string()),
            ("Class at the date".into(), v.get(t).map_or("-".into(), |&x| self.name_of(x))),
        ];
        // the first of the most frequent (std::max_element)
        let best = count.iter().fold(None::<(i32, usize)>, |b, (&k, &c)| match b {
            Some((_, bc)) if bc >= c => b,
            _ => Some((k, c)),
        });
        rows.push((
            "Majority class".into(),
            best.map_or("-".into(), |(k, c)| format!("{} {:.0}%", self.name_of(k as f32), 100.0 * c as f64 / n as f64)),
        ));
        rows.push(("Classes seen".into(), count.len().to_string()));
        rows.push(("Changes".into(), changes.to_string()));
        rows.push((
            "Last change".into(),
            match (last_change, prev) {
                (Some(lc), Some(p)) => {
                    format!("{}: {} -> {}", info.layers[lc].label, self.name_of(v[p]), self.name_of(v[lc]))
                }
                _ => "-".into(),
            },
        ));
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_values_only() {
        let mut c = BTreeMap::new();
        assert!(count_classes(&[1.0, 2.0, f32::NAN, 2.0], 1, 40, &mut c).is_some());
        assert_eq!(c, BTreeMap::from([(1, 1), (2, 2)]));
        assert!(count_classes(&[1.5], 1, 40, &mut BTreeMap::new()).is_none());
        let many: Vec<f32> = (0..50).map(|v| v as f32).collect();
        assert!(count_classes(&many, 1, 40, &mut BTreeMap::new()).is_none());
    }
}
