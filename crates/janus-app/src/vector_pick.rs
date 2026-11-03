//! The features of a vector layer by place: a grid of cells over the boxes of its features (active pixels), each
//! cell the features whose box touches it, so the feature under the cursor is looked for among a few instead of
//! all of them (a state's CAR: 700 us a place through 700 000 boxes, a few us through the cell's). Made with the
//! placement, on its thread.

/// Cells a feature's box may cover and still be listed in each; larger ones are listed apart (tried always).
const MAX_CELLS: usize = 16;

/// The cells (`n` x `n` over `bounds`), as offsets into `items`; the features too large for them.
#[derive(Default)]
pub struct BoxIndex {
    bounds: [f64; 4],
    n: usize,
    offsets: Vec<u32>,
    items: Vec<u32>,
    large: Vec<u32>,
}

impl BoxIndex {
    /// The features' boxes (x0, y0, x1, y1; x0 > x1: not placed).
    pub fn new(boxes: &[[f32; 4]]) -> BoxIndex {
        let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
        let mut placed = 0;
        for x in boxes.iter().filter(|x| x[0] <= x[2]) {
            b = [b[0].min(x[0].into()), b[1].min(x[1].into()), b[2].max(x[2].into()), b[3].max(x[3].into())];
            placed += 1;
        }
        if placed == 0 {
            return BoxIndex::default();
        }
        // about 4 features a cell, 1024 x 1024 cells at most
        let n = ((placed as f64 / 4.0).sqrt().ceil() as usize).clamp(1, 1024);
        let mut idx = BoxIndex { bounds: b, n, ..Default::default() };
        let mut counts = vec![0u32; n * n + 1];
        let mut large = Vec::new();
        // twice over the boxes: the cells' counts, then their features (offsets as a prefix sum)
        for (i, x) in boxes.iter().enumerate().filter(|(_, x)| x[0] <= x[2]) {
            let (cx0, cy0, cx1, cy1) = idx.cells_of(x[0].into(), x[1].into(), x[2].into(), x[3].into());
            if (cx1 - cx0 + 1) * (cy1 - cy0 + 1) > MAX_CELLS {
                large.push(i as u32);
                continue;
            }
            for cy in cy0..=cy1 {
                for cx in cx0..=cx1 {
                    counts[cy * n + cx + 1] += 1;
                }
            }
        }
        for k in 1..counts.len() {
            counts[k] += counts[k - 1];
        }
        let mut fill = counts.clone();
        let mut items = vec![0u32; counts[n * n] as usize];
        for (i, x) in boxes.iter().enumerate().filter(|(_, x)| x[0] <= x[2]) {
            let (cx0, cy0, cx1, cy1) = idx.cells_of(x[0].into(), x[1].into(), x[2].into(), x[3].into());
            if (cx1 - cx0 + 1) * (cy1 - cy0 + 1) > MAX_CELLS {
                continue;
            }
            for cy in cy0..=cy1 {
                for cx in cx0..=cx1 {
                    let c = cy * n + cx;
                    items[fill[c] as usize] = i as u32;
                    fill[c] += 1;
                }
            }
        }
        (idx.offsets, idx.items, idx.large) = (counts, items, large);
        idx
    }

    /// The cells (inclusive) a box touches, clamped to the grid.
    fn cells_of(&self, x0: f64, y0: f64, x1: f64, y1: f64) -> (usize, usize, usize, usize) {
        let b = self.bounds;
        let n = self.n as f64;
        let (w, h) = ((b[2] - b[0]).max(1e-9), (b[3] - b[1]).max(1e-9));
        let c = |v: f64, o: f64, s: f64| ((v - o) / s * n).floor().clamp(0.0, n - 1.0) as usize;
        (c(x0, b[0], w), c(y0, b[1], h), c(x1, b[0], w), c(y1, b[1], h))
    }

    /// The features whose box may touch the box (x0, y0, x1, y1), each once, in increasing order.
    pub fn query(&self, x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<usize> {
        let b = self.bounds;
        if self.n == 0 || x1 < b[0] || y1 < b[1] || x0 > b[2] || y0 > b[3] {
            return self.large.iter().map(|&i| i as usize).collect();
        }
        let (cx0, cy0, cx1, cy1) = self.cells_of(x0, y0, x1, y1);
        let mut out: Vec<usize> = self.large.iter().map(|&i| i as usize).collect();
        for cy in cy0..=cy1 {
            for cx in cx0..=cx1 {
                let c = cy * self.n + cx;
                out.extend(
                    self.items[self.offsets[c] as usize..self.offsets[c + 1] as usize].iter().map(|&i| i as usize),
                );
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_every_box_touching_a_place() {
        // a grid of small boxes, a few large ones across them, one not placed
        let mut boxes = Vec::new();
        for y in 0..60 {
            for x in 0..60 {
                let (x, y) = (x as f32 * 10.0, y as f32 * 10.0);
                boxes.push([x, y, x + 6.0, y + 6.0]);
            }
        }
        boxes.push([100.0, 100.0, 450.0, 300.0]);
        boxes.push([1.0, 1.0, 0.0, 0.0]);
        boxes.push([590.0, 0.0, 595.0, 599.0]);
        let idx = BoxIndex::new(&boxes);
        let mut s = 7u64;
        for _ in 0..2000 {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let x = (s >> 33) as f64 / (1u64 << 31) as f64 * 620.0 - 10.0;
            let y = (s >> 13 & 0xfffff) as f64 / (1u64 << 20) as f64 * 620.0 - 10.0;
            let t = 2.5;
            let want: Vec<usize> = (0..boxes.len())
                .filter(|&i| {
                    let b = boxes[i].map(f64::from);
                    b[0] <= b[2] && x + t >= b[0] && x - t <= b[2] && y + t >= b[1] && y - t <= b[3]
                })
                .collect();
            let got = idx.query(x - t, y - t, x + t, y + t);
            for i in &want {
                assert!(got.contains(i), "box {i} at ({x}, {y})");
            }
            assert!(got.windows(2).all(|w| w[0] < w[1]));
        }
        assert!(BoxIndex::new(&[]).query(0.0, 0.0, 1.0, 1.0).is_empty());
    }
}
