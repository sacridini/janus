//! A series' dates taken out of it, as layers of their own: one dragged out of the series in Layers (detached: the
//! series opened again without it), or all of them at once (Split into layers: a layer each, in a group named after
//! the series, which closes). Each new layer takes the series' colours, so dates side by side read alike. One file is
//! opened at a time (the app opens one series at a time): the steps wait in a queue, pumped each frame.

use std::collections::VecDeque;

use janus_cube::{BandSelection, CubeInfo};

use crate::app::State;
use crate::contents::Sel;
use crate::groups::LayerGroup;
use crate::style::LayerStyle;

/// What a layer just opened (or opened again) is given: a group to join (made with its first layer), the colours of
/// the series it came from.
#[derive(Clone, Default)]
pub struct AfterOpen {
    pub group: Option<(u32, String)>,
    pub style: Option<LayerStyle>,
    /// its name (a series opened again from fewer files keeps its own)
    pub name: Option<String>,
}

/// A step of a split or a detach.
pub enum LayerJob {
    /// a file (a date of the series) opened as a layer of its own
    Open { input: String, sel: BandSelection, name: String, after: AfterOpen },
    /// the series (by its cube's id) opened again from `inputs`, its group and colours kept
    Reopen { id: u64, inputs: Vec<String>, sel: BandSelection, after: AfterOpen },
    /// the series closed
    Close { id: u64 },
}

/// The steps waiting.
#[derive(Default)]
pub struct LayerJobs {
    pub queue: VecDeque<LayerJob>,
}

impl LayerJobs {
    /// The layers still to be opened (the overview's memory is shared with them).
    pub fn waiting_opens(&self) -> usize {
        self.queue.iter().filter(|j| matches!(j, LayerJob::Open { .. })).count()
    }
}

/// What opens date (band) t of a series alone: its file as the series reads it, or a band of a file of several
/// dates through GDAL's vrt:// connection (None: a NetCDF variable's level, or embeddings' dimensions).
pub(crate) fn step_input(info: &CubeInfo, t: usize) -> Option<(String, BandSelection)> {
    let l = info.layers.get(t)?;
    let one_file = info.layers.iter().all(|x| x.path == l.path);
    if info.multidim || l.band_base != 0 || (one_file && info.bands_per_date > 1) {
        return None;
    }
    if one_file {
        let path = l.path.replace('\\', "/");
        Some((format!("vrt://{path}?bands={}", l.band), BandSelection::default()))
    } else {
        Some((l.path.clone(), info.sel))
    }
}

/// The files of the series without date `t`'s, in order (None: the series is not a file a date, or `t` is its
/// only date). Several dates of one file (a date's bands) leave together.
pub(crate) fn inputs_without(info: &CubeInfo, t: usize) -> Option<Vec<String>> {
    let l = info.layers.get(t)?;
    if info.multidim || info.layers.iter().all(|x| x.path == l.path) {
        return None;
    }
    let mut out: Vec<String> = Vec::new();
    for x in &info.layers {
        if x.path != l.path && !out.contains(&x.path) {
            out.push(x.path.clone());
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Whether every date of the series opens alone (Split into layers).
pub(crate) fn splittable(info: &CubeInfo) -> bool {
    info.t() > 1 && (0..info.t()).all(|t| step_input(info, t).is_some())
}

impl State {
    /// The series' style for its dates opened alone: its ranges as it shows them now kept (set by hand), so that a
    /// date reads as it did in the series and dates side by side alike (each date's own range when the series
    /// stretches each date on its own).
    fn style_for_dates(&self, id: u64) -> Option<LayerStyle> {
        let mut style = self.style_of(Sel::Series(id))?;
        let i = self.series_index(id)?;
        let d = if i == self.active { &self.display } else { &self.layers[i].disp };
        if let LayerStyle::Series(st) = &mut style
            && !d.per_date_range
        {
            for (r, now) in st.ranges.iter_mut().zip(&d.ranges) {
                if now.is_set() {
                    r.2 = true;
                }
            }
        }
        Some(style)
    }

    /// Date (band) `t` of the series `id` dragged out of it: a layer of its own with the series' colours, the series
    /// opened again without it (its group and colours kept).
    pub(crate) fn detach_step(&mut self, id: u64, t: usize) {
        let Some(i) = self.series_index(id) else { return };
        let Some(info) = self.layer_session(i).map(|s| s.info.clone()) else { return };
        let (Some((input, sel)), Some(rest)) = (step_input(&info, t), inputs_without(&info, t)) else { return };
        let style = self.style_for_dates(id);
        let group = self.group_of(Sel::Series(id)).and_then(|g| self.groups.iter().find(|gr| gr.id == g));
        let group = group.map(|gr| (gr.id, gr.name.clone()));
        let name = format!("{} [{}]", self.layers[i].name, info.layers[t].label);
        let series_name = self.layers[i].name.clone();
        let after = AfterOpen { group: group.clone(), style, name: None };
        self.layer_jobs.queue.push_back(LayerJob::Open { input, sel, name, after });
        // the series keeps its name and colours, its ranges computed again unless set by hand
        let after = AfterOpen { group, style: self.style_of(Sel::Series(id)), name: Some(series_name) };
        self.layer_jobs.queue.push_back(LayerJob::Reopen { id, inputs: rest, sel: info.sel, after });
    }

    /// Every date (band) of the series `id` a layer of its own, in a group named after the series, with its colours;
    /// the series closed once they are open.
    pub(crate) fn split_series(&mut self, id: u64) {
        let Some(i) = self.series_index(id) else { return };
        let Some(info) = self.layer_session(i).map(|s| s.info.clone()) else { return };
        if !splittable(&info) {
            return;
        }
        let style = self.style_for_dates(id);
        self.next_group += 1;
        let group = Some((self.next_group, self.layers[i].name.clone()));
        for t in 0..info.t() {
            let Some((input, sel)) = step_input(&info, t) else { continue };
            let name = format!("{} [{}]", self.layers[i].name, info.layers[t].label);
            let after = AfterOpen { group: group.clone(), style: style.clone(), name: None };
            self.layer_jobs.queue.push_back(LayerJob::Open { input, sel, name, after });
        }
        self.layer_jobs.queue.push_back(LayerJob::Close { id });
    }

    /// The next steps, while no series is being opened.
    pub(crate) fn pump_layer_jobs(&mut self, ctx: &egui::Context) {
        while self.opening.is_none() {
            let Some(job) = self.layer_jobs.queue.pop_front() else { return };
            match job {
                LayerJob::Open { input, sel, name, after } => {
                    let mut o = crate::session::Opening::start_with(vec![input], true, sel, ctx);
                    o.name = Some(name);
                    o.after = after;
                    self.opening = Some(o);
                }
                LayerJob::Reopen { id, inputs, sel, after } => {
                    let Some(i) = self.series_index(id) else { continue };
                    self.reopen_layer_inputs(i, inputs, sel, ctx);
                    if let Some(o) = self.opening.as_mut() {
                        o.after = after;
                    }
                }
                LayerJob::Close { id } => {
                    if let Some(i) = self.series_index(id) {
                        self.remove_layer(i, ctx);
                    }
                }
            }
        }
    }

    /// A layer opened (or opened again) by a split or a detach: into its group, the series' colours given to it.
    pub(crate) fn after_open(&mut self, id: u64, after: AfterOpen) {
        let me = Sel::Series(id);
        if let Some((g, name)) = after.group {
            if self.groups.iter().any(|gr| gr.id == g) {
                self.set_group(me, Some(g));
            } else {
                self.set_group(me, None);
                self.groups.push(LayerGroup { id: g, name, members: vec![me] });
            }
        }
        if let Some(style) = after.style {
            self.apply_style(me, &style);
        }
        if let (Some(name), Some(i)) = (after.name, self.series_index(id)) {
            self.layers[i].name = name;
        }
        self.map.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn series(paths: &[&str]) -> CubeInfo {
        let layers = paths
            .iter()
            .enumerate()
            .map(|(k, p)| janus_cube::Layer {
                path: p.to_string(),
                band: 1,
                label: format!("200{k}"),
                ..Default::default()
            })
            .collect();
        CubeInfo { layers, ..Default::default() }
    }

    #[test]
    fn a_date_leaves_with_its_file() {
        let info = series(&["a.tif", "b.tif", "c.tif"]);
        assert_eq!(inputs_without(&info, 1), Some(vec!["a.tif".to_string(), "c.tif".to_string()]));
        assert_eq!(step_input(&info, 1).map(|x| x.0), Some("b.tif".to_string()));
        assert!(splittable(&info));
        // one date left: nothing to detach it from
        assert_eq!(inputs_without(&series(&["a.tif"]), 0), None);
        // the bands of one file: opened alone (a band of a VRT), but not taken out of it
        let one = series(&["s.tif", "s.tif"]);
        assert_eq!(inputs_without(&one, 0), None);
        assert!(step_input(&one, 1).is_some_and(|x| x.0.starts_with("vrt://s.tif?bands=")));
    }
}
