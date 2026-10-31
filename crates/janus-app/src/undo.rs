//! Global undo (phase 55): Ctrl+Z, Ctrl+Shift+Z (Ctrl+Y) and Edit > Undo / Redo. The layers' light state (the
//! project's, without pixels: project.rs) is compared at the end of a frame after a click, a key or every half
//! second; a change makes a step, the state before it and after. While a button is held (a slider, a layer
//! dragged) nothing is recorded, so a drag is one step. A step back to a state with a layer that was closed opens
//! it again (its files, band, style, date) as a project opens its layers; one without a layer closes it. Panning,
//! the date shown, the panels and the active layer alone are not steps. Opening a project starts the history again.

use std::time::Instant;

use crate::app::State;
use crate::contents::Sel;
use crate::order::Item;
use crate::project::Project;

/// The steps kept.
const MAX_STEPS: usize = 100;
/// The state looked at this often without a click or a key.
const CHECK_SECONDS: f64 = 0.5;

/// The state a step goes back to: the project's, without what is not undone (the view, the dates, the swipe and the
/// map panels, the basemap, the saved views, the season); each list's layers by their ids, in the project's order (a
/// layer closed and opened again gets new ids, remapped through the history).
#[derive(Clone, Debug)]
pub struct Snap {
    p: Project,
    series: Vec<u64>,
    vectors: Vec<u32>,
    results: Vec<u32>,
    /// each series' date (label, place): given back to a series opened again, not compared
    dates: Vec<(String, usize)>,
}

impl PartialEq for Snap {
    fn eq(&self, o: &Snap) -> bool {
        self.p == o.p && self.series == o.series && self.vectors == o.vectors && self.results == o.results
    }
}

impl Snap {
    /// The layer of a project's key ("s0", "v1", "r2").
    fn sel(&self, key: &str) -> Option<Sel> {
        let k: usize = key.get(1..)?.parse().ok()?;
        match key.as_bytes().first()? {
            b's' => self.series.get(k).map(|&id| Sel::Series(id)),
            b'v' => self.vectors.get(k).map(|&id| Sel::Vector(id)),
            b'r' => self.results.get(k).map(|&id| Sel::Result(id)),
            _ => None,
        }
    }

    /// Layer `from` known as `to` (opened again).
    fn remap(&mut self, from: Sel, to: Sel) {
        match (from, to) {
            (Sel::Series(a), Sel::Series(b)) => self.series.iter_mut().filter(|x| **x == a).for_each(|x| *x = b),
            (Sel::Vector(a), Sel::Vector(b)) => self.vectors.iter_mut().filter(|x| **x == a).for_each(|x| *x = b),
            (Sel::Result(a), Sel::Result(b)) => self.results.iter_mut().filter(|x| **x == a).for_each(|x| *x = b),
            _ => {}
        }
    }
}

/// A change: the state before and after it, and what it was ("Hide NDVI").
#[derive(Clone, Debug)]
pub struct Step {
    pub what: String,
    before: Snap,
    after: Snap,
}

/// A step being undone or redone: the layers it needs opened one at a time, then the rest.
pub struct Applying {
    target: Snap,
    next: usize,
    /// the series opening: its place in the target, the layers before it
    opening: Option<(usize, usize)>,
    vectors_asked: bool,
    vectors_before: Vec<u32>,
    results_asked: bool,
    results_before: Vec<u32>,
    /// the maps asked for: their place in the target, their file
    result_files: Vec<(usize, String)>,
    missing: Vec<String>,
}

#[derive(Default)]
pub struct UndoUi {
    pub past: Vec<Step>,
    pub future: Vec<Step>,
    /// the state as last recorded
    last: Option<Snap>,
    checked: Option<Instant>,
    pub applying: Option<Applying>,
}

impl UndoUi {
    /// A project closed or opened: no step to go back to.
    pub fn clear(&mut self) {
        *self = UndoUi::default();
    }

    pub fn busy(&self) -> bool {
        self.applying.is_some()
    }

    fn remap(&mut self, from: Sel, to: Sel) {
        for s in self.past.iter_mut().chain(self.future.iter_mut()) {
            s.before.remap(from, to);
            s.after.remap(from, to);
        }
        if let Some(l) = &mut self.last {
            l.remap(from, to);
        }
        if let Some(a) = &mut self.applying {
            a.target.remap(from, to);
        }
    }
}

/// What changed from `a` to `b`, as the Edit menu says it.
fn describe(a: &Snap, b: &Snap) -> String {
    let some = |n: usize, one: &str, many: &str| if n == 1 { one.to_string() } else { format!("{n} {many}") };
    // layers closed or opened
    let names = |s: &Snap, other: &Snap| -> Vec<String> {
        let mut out: Vec<String> = vec![];
        out.extend(
            s.series.iter().zip(&s.p.series).filter(|(id, _)| !other.series.contains(id)).map(|(_, e)| e.name.clone()),
        );
        out.extend(
            s.vectors
                .iter()
                .zip(&s.p.vectors)
                .filter(|(id, _)| !other.vectors.contains(id))
                .map(|(_, e)| e.name.clone()),
        );
        out.extend(
            s.results
                .iter()
                .zip(&s.p.results)
                .filter(|(id, _)| !other.results.contains(id))
                .map(|(_, e)| e.name.clone()),
        );
        out
    };
    let (closed, opened) = (names(a, b), names(b, a));
    if !closed.is_empty() {
        return format!(
            "Close {}",
            if closed.len() == 1 { closed[0].clone() } else { some(closed.len(), "", "layers") }
        );
    }
    if !opened.is_empty() {
        return format!(
            "Open {}",
            if opened.len() == 1 { opened[0].clone() } else { some(opened.len(), "", "layers") }
        );
    }
    let shown = |v: bool, name: &str| format!("{} {name}", if v { "Show" } else { "Hide" });
    for (x, y) in a.p.series.iter().zip(&b.p.series) {
        if x.visible != y.visible {
            return shown(y.visible, &y.name);
        }
        if x.name != y.name {
            return format!("Rename {}", x.name);
        }
        if x.style != y.style {
            let st = |t: &str| match crate::style::LayerStyle::from_text(t) {
                Some(crate::style::LayerStyle::Series(s)) => Some(*s),
                _ => None,
            };
            if let (Some(mut s), Some(t)) = (st(&x.style), st(&y.style)) {
                if s.opacity != t.opacity {
                    s.opacity = t.opacity;
                    if s.to_text_eq(&t) {
                        return format!("Opacity of {}", y.name);
                    }
                }
                if s.blend != t.blend {
                    s.blend = t.blend;
                    if s.to_text_eq(&t) {
                        return format!("Blend of {}", y.name);
                    }
                }
            }
            return format!("Style of {}", y.name);
        }
    }
    for (x, y) in a.p.vectors.iter().zip(&b.p.vectors) {
        if x.visible != y.visible {
            return shown(y.visible, &y.name);
        }
        if x.name != y.name {
            return format!("Rename {}", x.name);
        }
        if x.color != y.color {
            return format!("Colour of {}", y.name);
        }
    }
    for (x, y) in a.p.results.iter().zip(&b.p.results) {
        if x.visible != y.visible {
            return shown(y.visible, &y.name);
        }
        if x.opacity != y.opacity {
            return format!("Opacity of {}", y.name);
        }
        if x != y {
            return format!("Colours of {}", y.name);
        }
    }
    if a.p.order != b.p.order {
        return "Layer order".into();
    }
    if a.p.groups != b.p.groups {
        return "Groups".into();
    }
    if a.p.pins != b.p.pins || a.p.pin_classes != b.p.pin_classes || a.p.pin_class_list != b.p.pin_class_list {
        return "Pins".into();
    }
    if a.p.roi != b.p.roi || a.p.roi_polygon != b.p.roi_polygon {
        return "ROI".into();
    }
    if a.p.transect != b.p.transect {
        return "Transect".into();
    }
    "Change".into()
}

impl crate::style::SeriesStyle {
    /// The same style (as its text says it).
    fn to_text_eq(&self, o: &crate::style::SeriesStyle) -> bool {
        crate::style::LayerStyle::Series(Box::new(self.clone())).to_text()
            == crate::style::LayerStyle::Series(Box::new(o.clone())).to_text()
    }
}

impl State {
    /// The state undo goes back to, now.
    pub(crate) fn undo_snap(&mut self) -> Snap {
        let mut p = self.project_state(None);
        let idx: Vec<usize> =
            (0..self.layers.len()).filter(|&i| !self.layers[i].canvas && self.layer_session(i).is_some()).collect();
        let series: Vec<u64> = idx.iter().filter_map(|&i| self.layer_session(i).map(|s| s.info.id)).collect();
        let vectors = self.vectors.iter().map(|v| v.id).collect();
        let results =
            self.zeit.results.iter().filter(|r| !r.preview && series.contains(&r.cube)).map(|r| r.id).collect();
        let dates = p.series.iter_mut().map(|e| (std::mem::take(&mut e.date), std::mem::take(&mut e.t))).collect();
        p.view.clear();
        (p.swipe, p.map_views, p.basemap, p.saved_views, p.season) =
            (None, vec![], Default::default(), vec![], (false, 0, 0));
        Snap { p, series, vectors, results, dates }
    }

    /// The state undo compares, as text (the tests).
    pub(crate) fn undo_text(&mut self) -> String {
        serde_json::to_string_pretty(&self.undo_snap().p).unwrap_or_default()
    }

    /// Each frame's end: a change since the last state recorded becomes a step (not while a button is held, a
    /// layer opens, a project opens or a step is applied).
    pub(crate) fn pump_undo(&mut self, ctx: &egui::Context) {
        if self.undo.applying.is_some() {
            self.undo_apply_step(ctx);
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
            return;
        }
        if self.project.restoring() {
            return;
        }
        let (down, event) = ctx.input(|i| {
            let key =
                i.events.iter().any(|e| matches!(e, egui::Event::Key { pressed: true, .. } | egui::Event::Text(_)));
            (i.pointer.any_down(), key || i.pointer.any_released())
        });
        if down {
            return;
        }
        let now = Instant::now();
        if !event
            && self.undo.last.is_some()
            && self.undo.checked.is_some_and(|t| (now - t).as_secs_f64() < CHECK_SECONDS)
        {
            return;
        }
        self.record_undo();
    }

    /// The state now recorded: a step if it changed (layers still opening: later).
    pub(crate) fn record_undo(&mut self) {
        if self.opening.is_some() || !self.vector_loads.is_empty() || self.zeit.loads_pending() || self.undo.busy() {
            return;
        }
        self.undo.checked = Some(Instant::now());
        let snap = self.undo_snap();
        let Some(last) = self.undo.last.take() else {
            self.undo.last = Some(snap);
            return;
        };
        if snap == last {
            self.undo.last = Some(last);
            return;
        }
        // another layer made active (the same layers): the pins and the ROI follow its pixels, nothing to undo
        if last.p.active != snap.p.active
            && last.series == snap.series
            && last.vectors == snap.vectors
            && last.results == snap.results
        {
            self.undo.last = Some(snap);
            return;
        }
        let what = describe(&last, &snap);
        self.undo.past.push(Step { what, before: last, after: snap.clone() });
        if self.undo.past.len() > MAX_STEPS {
            self.undo.past.remove(0);
        }
        self.undo.future.clear();
        self.undo.last = Some(snap);
    }

    pub(crate) fn can_undo(&self) -> Option<&str> {
        (!self.undo.busy() && !self.project.restoring()).then(|| self.undo.past.last().map(|s| s.what.as_str()))?
    }

    pub(crate) fn can_redo(&self) -> Option<&str> {
        (!self.undo.busy() && !self.project.restoring()).then(|| self.undo.future.last().map(|s| s.what.as_str()))?
    }

    /// Ctrl+Z, Edit > Undo: the last step undone.
    pub(crate) fn undo(&mut self, ctx: &egui::Context) {
        // a change not recorded yet is the step undone
        self.record_undo();
        if self.can_undo().is_none() {
            return;
        }
        let Some(step) = self.undo.past.pop() else { return };
        let target = step.before.clone();
        self.undo.future.push(step);
        self.start_undo_apply(target, ctx);
    }

    /// Ctrl+Shift+Z, Ctrl+Y, Edit > Redo: the last step undone done again.
    pub(crate) fn redo(&mut self, ctx: &egui::Context) {
        self.record_undo();
        if self.can_redo().is_none() {
            return;
        }
        let Some(step) = self.undo.future.pop() else { return };
        let target = step.after.clone();
        self.undo.past.push(step);
        self.start_undo_apply(target, ctx);
    }

    /// Ctrl+Z, Ctrl+Shift+Z and Ctrl+Y (not while a text field has the keyboard, nor while a polygon is drawn).
    pub(crate) fn undo_shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers};
        if ctx.egui_wants_keyboard_input() || self.series.poly.on {
            return;
        }
        let chord = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_key(m, k));
        // with Shift first: a chord without it matches with it too
        if chord(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z) || chord(Modifiers::COMMAND, Key::Y) {
            self.redo(ctx);
        } else if chord(Modifiers::COMMAND, Key::Z) {
            self.undo(ctx);
        }
    }

    /// The Edit menu.
    pub(crate) fn edit_menu(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let undo = self.can_undo().map(|w| format!("Undo {w}"));
        let redo = self.can_redo().map(|w| format!("Redo {w}"));
        let b = egui::Button::new(undo.clone().unwrap_or("Undo".into())).shortcut_text("Ctrl+Z");
        if ui.add_enabled(undo.is_some(), b).clicked() {
            ui.close();
            self.undo(&ctx);
        }
        let b = egui::Button::new(redo.clone().unwrap_or("Redo".into())).shortcut_text("Ctrl+Shift+Z");
        if ui.add_enabled(redo.is_some(), b).clicked() {
            ui.close();
            self.redo(&ctx);
        }
    }

    /// The state `target` brought back: what it has not closed at once, what it has and is not open asked for.
    fn start_undo_apply(&mut self, target: Snap, ctx: &egui::Context) {
        let cur = self.undo_snap();
        for id in cur.results.iter().filter(|id| !target.results.contains(id)) {
            if let Some(k) = self.zeit.results.iter().position(|r| r.id == *id) {
                self.zeit.results.remove(k);
            }
        }
        for id in cur.vectors.iter().filter(|id| !target.vectors.contains(id)) {
            if let Some(j) = self.vector_index(*id) {
                self.remove_vector(j);
            }
        }
        for id in cur.series.iter().filter(|id| !target.series.contains(id)) {
            if let Some(i) = self.series_index(*id) {
                self.remove_layer(i, ctx);
            }
        }
        self.contents.selected.clear();
        self.map.dirty = true;
        self.undo.applying = Some(Applying {
            target,
            next: 0,
            opening: None,
            vectors_asked: false,
            vectors_before: vec![],
            results_asked: false,
            results_before: vec![],
            result_files: vec![],
            missing: vec![],
        });
        self.undo_apply_step(ctx);
    }

    /// One step of a state brought back: the next series to open again, then the vector layers, the maps, the rest.
    fn undo_apply_step(&mut self, ctx: &egui::Context) {
        if self.opening.is_some() {
            return;
        }
        let Some(mut a) = self.undo.applying.take() else { return };
        if let Some((k, before)) = a.opening.take() {
            let mut e = a.target.p.series[k].clone();
            if self.layers.len() > before {
                let i = self.layers.len() - 1;
                if let Some(new) = self.layer_session(i).map(|s| s.info.id) {
                    let old = a.target.series[k];
                    (e.date, e.t) = a.target.dates.get(k).cloned().unwrap_or_default();
                    self.apply_series_entry(i, &e, ctx);
                    a.target.remap(Sel::Series(old), Sel::Series(new));
                    self.undo.remap(Sel::Series(old), Sel::Series(new));
                }
            } else {
                a.missing.push(format!("{} (it could not be opened)", e.name));
                self.error = None;
            }
        }
        while a.next < a.target.series.len() {
            let k = a.next;
            a.next += 1;
            if self.series_index(a.target.series[k]).is_some() {
                continue;
            }
            let e = &a.target.p.series[k];
            let inputs: Vec<String> = e.inputs.iter().filter_map(|f| f.resolve(None)).collect();
            if inputs.is_empty() {
                a.missing.push(format!("{}: its files are gone", e.name));
                continue;
            }
            let mut o =
                crate::session::Opening::start_with(inputs, !self.layers.is_empty(), e.selection.band_selection(), ctx);
            o.name = Some(e.name.clone());
            self.opening = Some(o);
            a.opening = Some((k, self.layers.len()));
            self.undo.applying = Some(a);
            return;
        }
        if !a.vectors_asked {
            a.vectors_asked = true;
            a.vectors_before = self.vectors.iter().map(|v| v.id).collect();
            let mut files: Vec<String> = vec![];
            for (k, e) in a.target.p.vectors.iter().enumerate() {
                if self.vector_index(a.target.vectors[k]).is_some() {
                    continue;
                }
                match e.file.resolve(None) {
                    Some(f) if !files.contains(&f) => files.push(f),
                    Some(_) => {}
                    None => a.missing.push(format!("{}: {} not found", e.name, e.file.shown())),
                }
            }
            if !files.is_empty() {
                self.add_vector_files(files, ctx);
            }
            self.undo.applying = Some(a);
            return;
        }
        if !self.vector_loads.is_empty() {
            self.undo.applying = Some(a);
            return;
        }
        if !a.results_asked {
            a.results_asked = true;
            // the vector layers opened again: each the one of the target of its file and layer, the file's others
            // closed
            let new: Vec<u32> = self.vectors.iter().map(|v| v.id).filter(|id| !a.vectors_before.contains(id)).collect();
            for id in new {
                let Some(j) = self.vector_index(id) else { continue };
                let (path, layer) = (self.vectors[j].data.path.clone(), self.vectors[j].data.layer.clone());
                let k = (0..a.target.vectors.len()).find(|&k| {
                    let e = &a.target.p.vectors[k];
                    self.vector_index(a.target.vectors[k]).is_none()
                        && e.file.resolve(None).as_ref() == Some(&path)
                        && e.layer == layer
                });
                match k {
                    Some(k) => {
                        let old = a.target.vectors[k];
                        a.target.remap(Sel::Vector(old), Sel::Vector(id));
                        self.undo.remap(Sel::Vector(old), Sel::Vector(id));
                    }
                    None => self.remove_vector(j),
                }
            }
            a.results_before = self.zeit.results.iter().map(|r| r.id).collect();
            for (k, e) in a.target.p.results.iter().enumerate() {
                if self.zeit.results.iter().any(|r| r.id == a.target.results[k]) {
                    continue;
                }
                let Some(file) = e.file.resolve(None) else {
                    a.missing.push(format!("{}: {} not found", e.name, e.file.shown()));
                    continue;
                };
                let cube = a.target.series.get(e.series).copied();
                let Some((cube, info)) =
                    cube.and_then(|c| Some((c, self.layer_session(self.series_index(c)?)?.info.clone())))
                else {
                    continue;
                };
                a.result_files.push((k, file.clone()));
                let layer = crate::project::result_layer(e, file, cube, info, None);
                self.load_result_layer(layer, false, ctx);
            }
            self.undo.applying = Some(a);
            return;
        }
        if self.zeit.loads_pending() {
            self.undo.applying = Some(a);
            return;
        }
        for (k, path) in std::mem::take(&mut a.result_files) {
            let found = self.zeit.results.iter().find(|r| r.path == path && !a.results_before.contains(&r.id));
            if let Some(new) = found.map(|r| r.id) {
                let old = a.target.results[k];
                a.target.remap(Sel::Result(old), Sel::Result(new));
                self.undo.remap(Sel::Result(old), Sel::Result(new));
            }
        }
        self.apply_snap(&a.target, ctx);
        if !a.missing.is_empty() {
            self.notify("Undo could not open every layer", &a.missing.join("\n"), true);
        }
        self.undo.last = Some(self.undo_snap());
    }

    /// The target's state given to the layers open (each found by its id): names, visibility, styles, the order,
    /// the groups, the active layer, then the pins, the ROI and the transect in its pixels.
    fn apply_snap(&mut self, t: &Snap, ctx: &egui::Context) {
        let cur = self.undo_snap();
        for (k, e) in t.p.series.iter().enumerate() {
            let Some(i) = self.series_index(t.series[k]) else { continue };
            let sel = Sel::Series(t.series[k]);
            let now = self.style_of(sel).map(|s| crate::project::kept_style(s).to_text());
            if now.as_deref() != Some(e.style.as_str())
                && let Some(st) = crate::style::LayerStyle::from_text(&e.style)
            {
                self.apply_style(sel, &st);
            }
            let l = &mut self.layers[i];
            (l.name, l.visible) = (e.name.clone(), e.visible);
        }
        for (k, e) in t.p.vectors.iter().enumerate() {
            let Some(j) = self.vector_index(t.vectors[k]) else { continue };
            let v = &mut self.vectors[j];
            (v.name, v.visible) = (e.name.clone(), e.visible);
            v.color = egui::Color32::from_rgba_premultiplied(e.color[0], e.color[1], e.color[2], e.color[3]);
        }
        for (k, e) in t.p.results.iter().enumerate() {
            let Some(r) = self.zeit.results.iter_mut().find(|r| r.id == t.results[k]) else { continue };
            (r.name, r.lo, r.hi, r.visible, r.opacity) = (e.name.clone(), e.lo, e.hi, e.visible, e.opacity);
            r.cmap = janus_gpu::colormaps::by_name(&e.colormap).unwrap_or(r.cmap);
        }
        // the order: the target's, any other layer (a preview) on top, the canvas at the bottom
        let item = |s: &State, sel: Sel| -> Option<Item> {
            match sel {
                Sel::Series(id) => s.series_index(id).map(Item::Series),
                Sel::Vector(id) => s.vector_index(id).map(Item::Vector),
                Sel::Result(id) => s.zeit.results.iter().position(|r| r.id == id).map(Item::Result),
            }
        };
        let mut order: Vec<Item> = t.p.order.iter().filter_map(|k| t.sel(k)).filter_map(|s| item(self, s)).collect();
        for it in self.draw_order() {
            if !order.contains(&it) {
                order.push(it);
            }
        }
        if let Some(c) = self.canvas_index() {
            order.retain(|it| *it != Item::Series(c));
            order.insert(0, Item::Series(c));
        }
        self.set_draw_order(&order);
        if cur.p.groups != t.p.groups || cur.series != t.series || cur.vectors != t.vectors || cur.results != t.results
        {
            self.groups.clear();
            for g in &t.p.groups {
                let members: Vec<Sel> = g.members.iter().filter_map(|k| t.sel(k)).collect();
                if members.len() >= 2 {
                    self.next_group += 1;
                    self.groups.push(crate::groups::LayerGroup { id: self.next_group, name: g.name.clone(), members });
                }
            }
        }
        if let Some(Sel::Series(id)) = t.p.active.and_then(|k| t.sel(&format!("s{k}")))
            && let Some(i) = self.series_index(id)
            && i != self.active
        {
            self.set_active(i, ctx);
        }
        let cur = self.undo_snap();
        if cur.p.pins != t.p.pins || cur.p.pin_classes != t.p.pin_classes || cur.p.pin_class_list != t.p.pin_class_list
        {
            self.clear_pins();
            self.restore_pins(&t.p.pins, &t.p.pin_classes, &t.p.pin_class_list);
        }
        if cur.p.roi != t.p.roi || cur.p.roi_polygon != t.p.roi_polygon {
            match t.p.roi {
                Some([x0, y0, x1, y1]) => {
                    self.set_roi_shape([x0 as i64, y0 as i64, x1 as i64, y1 as i64], t.p.roi_polygon.clone())
                }
                None => self.clear_roi(),
            }
        }
        if cur.p.transect != t.p.transect {
            if t.p.transect.len() >= 2 {
                self.set_transect(t.p.transect.clone());
            } else {
                self.clear_transect();
            }
        }
        self.map.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::SeriesEntry;

    fn snap(names: &[(&str, bool)]) -> Snap {
        let p = Project {
            series: names
                .iter()
                .map(|&(n, v)| SeriesEntry { name: n.into(), visible: v, ..Default::default() })
                .collect(),
            order: (0..names.len()).map(|k| format!("s{k}")).collect(),
            ..Default::default()
        };
        let series = (1..=names.len() as u64).collect();
        Snap { p, series, vectors: vec![], results: vec![], dates: vec![] }
    }

    #[test]
    fn steps_described() {
        let a = snap(&[("ndvi", true), ("landsat", true)]);
        let mut b = a.clone();
        b.p.series[1].visible = false;
        assert_eq!(describe(&a, &b), "Hide landsat");
        // closed: the layer gone from the list
        let mut c = snap(&[("ndvi", true)]);
        c.p.order = vec!["s0".into()];
        assert_eq!(describe(&a, &c), "Close landsat");
        assert_eq!(describe(&c, &a), "Open landsat");
        let mut d = a.clone();
        d.p.order.reverse();
        assert_eq!(describe(&a, &d), "Layer order");
        let mut e = a.clone();
        e.p.pins.push([3, 4]);
        assert_eq!(describe(&a, &e), "Pins");
        // the dates are not compared; a layer opened again is remapped to its new id
        let mut f = a.clone();
        f.dates = vec![("2020".into(), 3)];
        assert_eq!(f, a);
        f.remap(Sel::Series(2), Sel::Series(9));
        assert_eq!(f.sel("s1"), Some(Sel::Series(9)));
    }
}
