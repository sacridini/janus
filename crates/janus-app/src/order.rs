//! The drawing order, as in a GIS: series, the maps the runs made and vector layers in one order, bottom to top,
//! each layer's `z` its place in it. `layers`, `zeit.results` and `vectors` are each kept sorted by `z`, so each
//! list's own order is still its drawing order. A new series (or a run's map) goes over the top raster but under the
//! vector layers above it (rasters under vectors when added, as ArcGIS arranges them); a new vector layer on top of
//! everything. Any layer can then be moved anywhere. The canvas of vector layers alone stays at the bottom (z 0).

use crate::app::State;

/// A layer of the drawing order: a series (an index into `layers`), a vector layer (into `vectors`) or a map a run
/// made (into `zeit.results`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Item {
    Series(usize),
    Vector(usize),
    Result(usize),
}

/// The permutation that sorts `z` (stable), None if it is sorted.
fn sorting(z: &[u64]) -> Option<Vec<usize>> {
    let mut p: Vec<usize> = (0..z.len()).collect();
    p.sort_by_key(|&i| z[i]);
    p.iter().enumerate().any(|(k, &i)| k != i).then_some(p)
}

/// `v` in the order of the permutation `p`.
fn permute<T>(v: &mut Vec<T>, p: &[usize]) {
    let mut old: Vec<Option<T>> = std::mem::take(v).into_iter().map(Some).collect();
    *v = p.iter().filter_map(|&i| old[i].take()).collect();
}

impl State {
    /// A place on top of every layer, for a new one.
    pub fn take_z(&mut self) -> u64 {
        self.next_z += 1;
        self.next_z
    }

    /// Every layer, bottom to top (at the same `z`: series, then vector layers, then results).
    pub fn draw_order(&self) -> Vec<Item> {
        let mut out: Vec<(u64, Item)> = Vec::with_capacity(self.layers.len() + self.vectors.len());
        out.extend(self.layers.iter().enumerate().map(|(i, l)| (l.z, Item::Series(i))));
        out.extend(self.vectors.iter().enumerate().map(|(j, v)| (v.z, Item::Vector(j))));
        out.extend(self.zeit.results.iter().enumerate().map(|(k, r)| (r.z, Item::Result(k))));
        // stable: each list's order kept
        out.sort_by_key(|&(z, it)| (z, matches!(it, Item::Vector(_)) as u8 + 2 * matches!(it, Item::Result(_)) as u8));
        out.into_iter().map(|(_, it)| it).collect()
    }

    /// The drawing order set to `order` (every layer once, bottom to top): the lists sorted by it, the active
    /// layer kept.
    pub fn set_draw_order(&mut self, order: &[Item]) {
        for (k, it) in order.iter().enumerate() {
            let z = k as u64 + 1;
            match *it {
                Item::Series(i) => {
                    let l = &mut self.layers[i];
                    l.z = if l.canvas { 0 } else { z };
                }
                Item::Vector(j) => self.vectors[j].z = z,
                Item::Result(k) => self.zeit.results[k].z = z,
            }
        }
        self.next_z = self.next_z.max(order.len() as u64);
        if let Some(p) = sorting(&self.layers.iter().map(|l| l.z).collect::<Vec<_>>()) {
            self.active = p.iter().position(|&i| i == self.active).unwrap_or(self.active);
            permute(&mut self.layers, &p);
        }
        if let Some(p) = sorting(&self.vectors.iter().map(|v| v.z).collect::<Vec<_>>()) {
            permute(&mut self.vectors, &p);
            self.hover_vector = None;
        }
        if let Some(p) = sorting(&self.zeit.results.iter().map(|r| r.z).collect::<Vec<_>>()) {
            permute(&mut self.zeit.results, &p);
        }
        self.map.dirty = true;
    }

    /// The layer at `from` in the drawing order (bottom to top) moved to `to`, the others shifting.
    pub fn move_in_order(&mut self, from: usize, to: usize) {
        let mut o = self.draw_order();
        if from == to || from >= o.len() || to >= o.len() {
            return;
        }
        let it = o.remove(from);
        o.insert(to, it);
        self.set_draw_order(&o);
    }

    /// The series just added (the last one, on top of everything) put over the top raster, under the vector
    /// layers above it.
    pub(crate) fn place_new_series(&mut self) {
        let mut o = self.draw_order();
        let Some(new) = o.pop() else { return };
        if new != Item::Series(self.layers.len() - 1) {
            return;
        }
        let at = o.iter().rposition(|it| !matches!(it, Item::Vector(_))).map_or(0, |k| k + 1);
        o.insert(at, new);
        self.set_draw_order(&o);
    }

    /// A map a run made, as a layer of its own: over the top raster, under the vector layers above it; among the
    /// maps of its run already there by its place in the run's outputs (the first output on top).
    pub fn push_result(&mut self, mut r: crate::zeit::ResultLayer) {
        self.zeit.next_result += 1;
        r.id = self.zeit.next_result;
        if let (Some(s), Some(gpu), Some(b)) = (&self.session, &self.gpu, r.grid.clone()) {
            crate::layers::place_on(&mut r.place, &s.info, &b, gpu);
        }
        let (run, output) = (r.run.clone(), r.output);
        r.z = u64::MAX;
        self.zeit.results.push(r);
        let mut o = self.draw_order();
        let Some(new) = o.pop() else { return };
        let results = &self.zeit.results;
        let sibling = |it: &Item| matches!(*it, Item::Result(k) if !run.is_empty() && results[k].run == run);
        let at = match o.iter().position(sibling) {
            // under the run's first output listed after it, else over the run's top one
            Some(_) => o
                .iter()
                .position(|it| sibling(it) && matches!(*it, Item::Result(k) if results[k].output < output))
                .unwrap_or_else(|| o.iter().rposition(sibling).map_or(0, |k| k + 1)),
            None => o.iter().rposition(|it| !matches!(it, Item::Vector(_))).map_or(0, |k| k + 1),
        };
        o.insert(at, new);
        self.set_draw_order(&o);
    }
}
