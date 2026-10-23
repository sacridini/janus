//! The drawing order, as in a GIS: series and vector layers in one order, bottom to top, each layer's `z` its place
//! in it. `layers` and `vectors` are each kept sorted by `z`, so each list's own order is still its drawing order.
//! A new series goes over the top series but under the vector layers above it (rasters under vectors when added,
//! as ArcGIS arranges them); a new vector layer on top of everything. Any layer can then be moved anywhere. The
//! canvas of vector layers alone stays at the bottom (z 0).

use crate::app::State;

/// A layer of the drawing order: a series (an index into `layers`) or a vector layer (into `vectors`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Item {
    Series(usize),
    Vector(usize),
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

    /// Every layer, bottom to top.
    pub fn draw_order(&self) -> Vec<Item> {
        let (n, m) = (self.layers.len(), self.vectors.len());
        let mut out = Vec::with_capacity(n + m);
        let (mut i, mut j) = (0, 0);
        while i < n || j < m {
            if j >= m || (i < n && self.layers[i].z <= self.vectors[j].z) {
                out.push(Item::Series(i));
                i += 1;
            } else {
                out.push(Item::Vector(j));
                j += 1;
            }
        }
        out
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

    /// The series just added (the last one, on top of everything) put over the top series, under the vector
    /// layers above it.
    pub(crate) fn place_new_series(&mut self) {
        let mut o = self.draw_order();
        let Some(new) = o.pop() else { return };
        if new != Item::Series(self.layers.len() - 1) {
            return;
        }
        let at = o.iter().rposition(|it| matches!(it, Item::Series(_))).map_or(0, |k| k + 1);
        o.insert(at, new);
        self.set_draw_order(&o);
    }
}
