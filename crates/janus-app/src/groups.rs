//! Groups of layers in the Layers panel (phase 50): a name over a run of layers, series and vector layers alike,
//! next to each other in the drawing order. Its row on the list shows or hides them all, folds them, and its menu
//! renames it, moves the run to the top or the bottom, ungroups or closes them. A layer dragged onto a member joins
//! the group; one moved out of the run by its Move menu leaves it.

use crate::app::State;
use crate::contents::Sel;
use crate::order::Item;

#[derive(Clone, Debug, PartialEq)]
pub struct LayerGroup {
    pub id: u32,
    pub name: String,
    /// its layers (by the selection's ids)
    pub members: Vec<Sel>,
}

impl State {
    /// An item of the drawing order as the list's selection knows it (None: the canvas of vector layers).
    pub(crate) fn sel_of(&self, it: Item) -> Option<Sel> {
        match it {
            Item::Series(i) => {
                let l = self.layers.get(i)?;
                (!l.canvas).then(|| self.layer_session(i).map(|s| Sel::Series(s.info.id)))?
            }
            Item::Vector(j) => self.vectors.get(j).map(|v| Sel::Vector(v.id)),
            Item::Result(k) => self.zeit.results.get(k).map(|r| Sel::Result(r.id)),
        }
    }

    pub(crate) fn group_of(&self, s: Sel) -> Option<u32> {
        self.groups.iter().find(|g| g.members.contains(&s)).map(|g| g.id)
    }

    /// Layer `s` into group `g` (None: out of any).
    pub(crate) fn set_group(&mut self, s: Sel, g: Option<u32>) {
        for gr in &mut self.groups {
            if Some(gr.id) != g {
                gr.members.retain(|&m| m != s);
            } else if !gr.members.contains(&s) {
                gr.members.push(s);
            }
        }
        self.groups.retain(|g| !g.members.is_empty());
    }

    /// The layers selected on the list (at least one) made a group, next to each other where the top one was;
    /// its id.
    pub(crate) fn group_selected(&mut self) -> Option<u32> {
        let o = self.draw_order();
        let sels: Vec<Option<Sel>> = o.iter().map(|&it| self.sel_of(it)).collect();
        let chosen: Vec<bool> = sels.iter().map(|s| s.is_some_and(|s| self.contents.selected.contains(&s))).collect();
        let picked = |k: usize| chosen[k];
        let top = (0..o.len()).rev().find(|&k| picked(k))?;
        let members: Vec<Item> = (0..o.len()).filter(|&k| picked(k)).map(|k| o[k]).collect();
        let mut rest: Vec<Item> = (0..o.len()).filter(|&k| !picked(k)).map(|k| o[k]).collect();
        let at = (0..top).filter(|&k| !picked(k)).count();
        rest.splice(at..at, members.iter().copied());
        self.set_draw_order(&rest);
        let members: Vec<Sel> = (0..o.len()).filter(|&k| picked(k)).filter_map(|k| sels[k]).collect();
        self.next_group += 1;
        let id = self.next_group;
        let n = self.groups.len() + 1;
        for gr in &mut self.groups {
            gr.members.retain(|m| !members.contains(m));
        }
        self.groups.retain(|g| !g.members.is_empty());
        self.groups.push(LayerGroup { id, name: format!("Group {n}"), members });
        Some(id)
    }

    pub(crate) fn ungroup(&mut self, g: u32) {
        self.groups.retain(|gr| gr.id != g);
    }

    /// Whether any of group `g`'s layers is shown.
    pub(crate) fn group_visible(&self, g: u32) -> bool {
        let Some(gr) = self.groups.iter().find(|gr| gr.id == g) else { return false };
        gr.members.iter().any(|&m| match m {
            Sel::Series(id) => self.series_index(id).is_some_and(|i| self.layers[i].visible),
            Sel::Vector(id) => self.vectors.iter().any(|v| v.id == id && v.visible),
            Sel::Result(id) => self.zeit.results.iter().any(|r| r.id == id && r.visible),
        })
    }

    /// Group `g`'s layers shown or hidden.
    pub(crate) fn set_group_visible(&mut self, g: u32, visible: bool) {
        let Some(gr) = self.groups.iter().find(|gr| gr.id == g) else { return };
        for m in gr.members.clone() {
            match m {
                Sel::Series(id) => {
                    if let Some(i) = self.series_index(id) {
                        self.layers[i].visible = visible;
                    }
                }
                Sel::Vector(id) => {
                    if let Some(v) = self.vectors.iter_mut().find(|v| v.id == id) {
                        v.visible = visible;
                    }
                }
                Sel::Result(id) => {
                    if let Some(r) = self.zeit.results.iter_mut().find(|r| r.id == id) {
                        r.visible = visible;
                    }
                }
            }
        }
        self.map.dirty = true;
    }

    /// Group `g`'s layers moved, together, to the top of the drawing order or to its bottom (over the canvas).
    pub(crate) fn move_group(&mut self, g: u32, top: bool) {
        let Some(gr) = self.groups.iter().find(|gr| gr.id == g) else { return };
        let o = self.draw_order();
        let (mut members, mut rest) = (vec![], vec![]);
        for &it in &o {
            if self.sel_of(it).is_some_and(|s| gr.members.contains(&s)) {
                members.push(it);
            } else {
                rest.push(it);
            }
        }
        let at = if top {
            rest.len()
        } else {
            usize::from(matches!(rest.first(), Some(Item::Series(i)) if self.layers[*i].canvas))
        };
        rest.splice(at..at, members);
        self.set_draw_order(&rest);
    }

    /// Group `g`'s layers selected (its row clicked): a run of rows Delete closes.
    pub(crate) fn select_group(&mut self, g: u32) {
        let Some(gr) = self.groups.iter().find(|gr| gr.id == g) else { return };
        self.contents.selected = gr.members.iter().copied().collect();
        self.contents.focused = true;
    }

    /// After the layer at `from` moved to `to` in the drawing order: dropped onto a row (`dropped`), it joins that
    /// row's group (or none); moved by its menu, it joins a group it landed inside of and leaves its own when no
    /// neighbour is in it.
    pub(crate) fn regroup_moved(&mut self, moved: Sel, target: Option<Sel>, dropped: bool) {
        if dropped {
            let g = target.and_then(|t| self.group_of(t));
            self.set_group(moved, g);
            return;
        }
        let o = self.draw_order();
        let Some(p) = o.iter().position(|&it| self.sel_of(it) == Some(moved)) else { return };
        let near =
            |k: Option<usize>| k.and_then(|k| o.get(k)).and_then(|&it| self.sel_of(it)).and_then(|s| self.group_of(s));
        let (below, above) = (near(p.checked_sub(1)), near(Some(p + 1)));
        let mine = self.group_of(moved);
        if below.is_some() && below == above {
            self.set_group(moved, below);
        } else if mine.is_some() && mine != below && mine != above {
            self.set_group(moved, None);
        }
    }

    /// The groups' layers still open (a layer closed leaves its group; a group left empty goes).
    pub(crate) fn tidy_groups(&mut self, open: &[Sel]) {
        for g in &mut self.groups {
            g.members.retain(|m| open.contains(m));
        }
        self.groups.retain(|g| !g.members.is_empty());
    }
}
