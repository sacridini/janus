//! The map's Similarity mode: every pixel's series against a reference series (the cursor's, a pin's or the ROI's
//! mean), as the embeddings' similarity to a reference, computed on the GPU over the dates of the overview
//! (display.wgsl, similarity). Two measures: the values (1 - their RMS difference over the layer's range of values:
//! 1 the same values at every date) and the shape (the curves' correlation: alike whatever their level).

use std::hash::{DefaultHasher, Hash, Hasher};

use egui::{ComboBox, Ui};
use janus_gpu::Mode;

use crate::app::State;
use crate::display::Similarity;

impl State {
    /// The reference series of the similarity, sent to the GPU when it changes: kept when the cursor leaves the map.
    pub(crate) fn update_similarity_ref(&mut self) {
        let in_view = self.views.iter().any(|v| v.own_mode && v.mode == Mode::Similarity);
        if self.display.mode != Mode::Similarity && !in_view {
            return;
        }
        let Some(s) = &self.session else { return };
        let t_n = s.info.t();
        let sim = &self.display.sim;
        let sr = &self.series;
        let (values, text): (Option<&[f32]>, String) = match sim.kind {
            0 => match &sr.hover {
                Some(h) => (Some(&h.values[..]), format!("the cursor ({}, {})", h.x, h.y)),
                None => (None, String::new()),
            },
            1 => match sr.pins.iter().find(|p| p.id == sim.pin).or(sr.pins.first()) {
                Some(p) => (Some(&p.values[..]), format!("pin {}", p.id)),
                None => (None, String::new()),
            },
            _ => (Some(&sr.roi.mean[..]), "the ROI's mean".to_string()),
        };
        let Some(values) = values.filter(|v| v.len() == t_n && v.iter().any(|x| !x.is_nan())) else { return };
        // the unit of the values' difference: the layer's range over every date (once a layer)
        let layers_done = s.s.overview.layers_done();
        let scale_key = (s.info.id, layers_done);
        if self.sim_scale_for != Some(scale_key) {
            let r = self.display.auto_range_of(Mode::Value, s).unwrap_or((0.0, 1.0));
            self.display.sim.scale = (r.1 - r.0).abs().max(1e-12);
            self.sim_scale_for = Some(scale_key);
        }
        let mut h = DefaultHasher::new();
        (s.info.id, self.display.sim.scale.to_bits()).hash(&mut h);
        for v in values {
            v.to_bits().hash(&mut h);
        }
        let key = h.finish();
        if key == self.display.sim.key {
            return;
        }
        let values = values.to_vec();
        if let Some(cube) = &s.cube {
            cube.set_reference(&values, self.display.sim.scale);
        }
        let sim = &mut self.display.sim;
        (sim.values, sim.text, sim.key) = (values, text, key);
        self.map.dirty = true;
    }

    /// What the map's title and status say of the similarity.
    pub(crate) fn similarity_text(&self) -> String {
        let sim = &self.display.sim;
        let measure = if sim.shape { "shape" } else { "values" };
        if sim.values.is_empty() {
            format!("Similarity ({measure})")
        } else {
            format!("Similarity to {} ({measure})", sim.text)
        }
    }
}

/// The Display panel's options of the similarity: its reference (the cursor, the `pins`, the ROI's mean if there is
/// one) and its measure.
pub(crate) fn options_ui(ui: &mut Ui, sim: &mut Similarity, pins: &[u32], roi: bool) -> bool {
    let mut dirty = false;
    let name = |kind: usize, pin: u32| match kind {
        0 => "The cursor".to_string(),
        1 => format!("Pin {pin}"),
        _ => "The ROI's mean".to_string(),
    };
    if sim.kind == 1 && !pins.contains(&sim.pin) {
        sim.pin = pins.first().copied().unwrap_or(0);
    }
    ui.horizontal(|ui| {
        ComboBox::from_id_salt("similarity reference")
            .width(ui.available_width() - 70.0)
            .selected_text(name(sim.kind, sim.pin))
            .show_ui(ui, |ui| {
                let mut pick = |ui: &mut Ui, kind: usize, pin: u32| {
                    if ui.selectable_label(sim.kind == kind && (kind != 1 || sim.pin == pin), name(kind, pin)).clicked()
                    {
                        (sim.kind, sim.pin, sim.key) = (kind, pin, 0);
                        dirty = true;
                    }
                };
                pick(ui, 0, 0);
                for &p in pins {
                    pick(ui, 1, p);
                }
                if roi {
                    pick(ui, 2, 0);
                }
            })
            .response
            .on_hover_text("Whose series every pixel's is compared with (the cursor's follows the mouse)");
        ui.label("Reference");
    });
    ui.horizontal(|ui| {
        ui.label("Compare");
        dirty |= ui
            .radio_value(&mut sim.shape, false, "Values")
            .on_hover_text(
                "1 - the RMS difference of the two series over the layer's range of values: 1 the same values at \
                 every date, 0.5 half the range apart",
            )
            .changed();
        dirty |= ui
            .radio_value(&mut sim.shape, true, "Shape")
            .on_hover_text(
                "The correlation of the two series: 1 the same curve whatever its level (a crop's calendar on \
                 poorer and richer soils), 0 unrelated, below 0 opposite",
            )
            .changed();
    });
    let what = if sim.values.is_empty() { "no reference yet".to_string() } else { format!("to {}", sim.text) };
    ui.weak(format!(
        "Every pixel's series {what}, over the dates both have (overview).{}",
        if sim.shape { "" } else { "\nUnit: the layer's range of values, 2-98 %." }
    ));
    dirty
}
