//! Plane geometry shared by the polygonal ROI and the vector layers (src/geom.cpp): polygons as rings of (x, y)
//! pairs, filled by the even-odd rule (so holes and parts of a multipolygon need no flags: a hole is one more ring
//! inside its shell).

/// x0, y0, x1, y1, ... (closed implicitly)
pub type Ring = Vec<f64>;
/// rings, even-odd
pub type Polygon = Vec<Ring>;

/// Bounding box (x0, y0, x1, y1) of every vertex; None without any.
pub fn bounds(p: &[Ring]) -> Option<[f64; 4]> {
    let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    for r in p {
        for v in r.as_chunks::<2>().0 {
            b[0] = b[0].min(v[0]);
            b[1] = b[1].min(v[1]);
            b[2] = b[2].max(v[0]);
            b[3] = b[3].max(v[1]);
        }
    }
    (b[0] <= b[2]).then_some(b)
}

fn in_ring(r: &[f64], x: f64, y: f64) -> bool {
    let n = r.len() / 2;
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi, xj, yj) = (r[i * 2], r[i * 2 + 1], r[j * 2], r[j * 2 + 1]);
        if (yi <= y) != (yj <= y) && x < xi + (y - yi) * (xj - xi) / (yj - yi) {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Even-odd: inside an odd number of rings.
pub fn contains(p: &[Ring], x: f64, y: f64) -> bool {
    p.iter().filter(|r| r.len() >= 6 && in_ring(r, x, y)).count() % 2 == 1
}

fn ring_area(r: &[f64]) -> f64 {
    let n = r.len() / 2;
    let mut a = 0.0;
    let mut j = n - 1;
    for i in 0..n {
        a += (r[j * 2] - r[i * 2]) * (r[j * 2 + 1] + r[i * 2 + 1]);
        j = i;
    }
    a.abs() * 0.5
}

/// Area enclosed (even-odd rings: shells minus holes; as their signed areas would not tell which is which, the
/// absolute areas of rings inside an even number of others count positive, the rest negative).
pub fn area(p: &[Ring]) -> f64 {
    let mut total = 0.0;
    for (k, r) in p.iter().enumerate() {
        if r.len() < 6 {
            continue;
        }
        // rings this one lies in (tested at its first vertex)
        let depth = p.iter().enumerate().filter(|&(m, o)| m != k && o.len() >= 6 && in_ring(o, r[0], r[1])).count();
        total += if depth % 2 == 0 { 1.0 } else { -1.0 } * ring_area(r);
    }
    total
}

/// A point inside a polygon (even-odd rings), found as GEOS's InteriorPointArea finds one: on a horizontal line
/// through the middle of its box, moved halfway to the nearest vertices above and below (so it meets no vertex),
/// the middle of the widest stretch inside. Parts the line misses (islands around a gap): the line through each
/// ring's box instead, the largest ring first. None if no line crosses a ring.
pub fn interior_point(p: &[Ring]) -> Option<(f64, f64)> {
    let boxes: Vec<Option<[f64; 4]>> = p.iter().map(|r| bounds(std::slice::from_ref(r))).collect();
    let b = bounds(p)?;
    if let Some(q) = widest_stretch(p, &boxes, p, b) {
        return Some(q);
    }
    let mut by_size: Vec<usize> = (0..p.len()).filter(|&k| boxes[k].is_some()).collect();
    let size = |k: usize| boxes[k].map_or(0.0, |b| (b[2] - b[0]) * (b[3] - b[1]));
    by_size.sort_by(|&a, &c| size(c).total_cmp(&size(a)));
    by_size.into_iter().find_map(|k| widest_stretch(p, &boxes, std::slice::from_ref(&p[k]), boxes[k]?))
}

/// The middle of the widest stretch of `p` inside on the line through the middle of `b`, moved off the vertices
/// of `near` (`boxes`: each ring's, to skip the ones the line misses).
fn widest_stretch(p: &[Ring], boxes: &[Option<[f64; 4]>], near: &[Ring], b: [f64; 4]) -> Option<(f64, f64)> {
    let c = (b[1] + b[3]) / 2.0;
    let (mut lo, mut hi) = (b[1], b[3]);
    for r in near {
        for v in r.as_chunks::<2>().0 {
            if v[1] <= c && v[1] > lo {
                lo = v[1];
            } else if v[1] > c && v[1] < hi {
                hi = v[1];
            }
        }
    }
    let y = (lo + hi) / 2.0;
    let mut xs = Vec::new();
    for (r, rb) in p.iter().zip(boxes) {
        if rb.is_none_or(|rb| y < rb[1] || y > rb[3]) {
            continue;
        }
        let v = r.as_chunks::<2>().0;
        for i in 0..v.len() {
            let (a, e) = (v[i], v[(i + 1) % v.len()]);
            if (a[1] > y) != (e[1] > y) {
                xs.push(a[0] + (y - a[1]) * (e[0] - a[0]) / (e[1] - a[1]));
            }
        }
    }
    xs.sort_by(f64::total_cmp);
    xs.as_chunks::<2>()
        .0
        .iter()
        .max_by(|s, t| (s[1] - s[0]).total_cmp(&(t[1] - t[0])))
        .map(|s| ((s[0] + s[1]) / 2.0, y))
}

/// The centroid of lines (open rings): their segments' middles weighted by their lengths (their vertices' mean if
/// they have no length), as GEOS's.
pub fn line_centroid(p: &[Ring]) -> Option<(f64, f64)> {
    let (mut sx, mut sy, mut sl) = (0.0, 0.0, 0.0);
    let (mut mx, mut my, mut n) = (0.0, 0.0, 0usize);
    for r in p {
        let v = r.as_chunks::<2>().0;
        for q in v {
            (mx, my, n) = (mx + q[0], my + q[1], n + 1);
        }
        for s in v.windows(2) {
            let l = (s[1][0] - s[0][0]).hypot(s[1][1] - s[0][1]);
            sx += l * (s[0][0] + s[1][0]) / 2.0;
            sy += l * (s[0][1] + s[1][1]) / 2.0;
            sl += l;
        }
    }
    if sl > 0.0 { Some((sx / sl, sy / sl)) } else { (n > 0).then(|| (mx / n as f64, my / n as f64)) }
}

/// Every vertex through f (in place; f returns None where it cannot); rings left with fewer than 3 points dropped.
pub fn transform(p: &mut Polygon, f: impl Fn(f64, f64) -> Option<(f64, f64)>) {
    for r in p.iter_mut() {
        *r = r.as_chunks::<2>().0.iter().filter_map(|v| f(v[0], v[1])).flat_map(|(x, y)| [x, y]).collect();
    }
    p.retain(|r| r.len() >= 6);
}

/// Each edge split so no piece is longer than `max_len` (before a curved transformation).
pub fn densify(p: &[Ring], max_len: f64) -> Polygon {
    p.iter()
        .map(|r| {
            let n = r.len() / 2;
            let mut d = Vec::new();
            for i in 0..n {
                let (ax, ay, bx, by) = (r[i * 2], r[i * 2 + 1], r[(i + 1) % n * 2], r[(i + 1) % n * 2 + 1]);
                let k = (((bx - ax).hypot(by - ay) / max_len).ceil() as i64).max(1);
                for s in 0..k {
                    d.push(ax + (bx - ax) * s as f64 / k as f64);
                    d.push(ay + (by - ay) * s as f64 / k as f64);
                }
            }
            d
        })
        .collect()
}

/// Cells of a bw x bh grid laid over the window (x0, y0, w, h) whose centres are inside (one scanline per row).
pub fn mask(p: &[Ring], x0: f64, y0: f64, w: f64, h: f64, bw: usize, bh: usize) -> Vec<bool> {
    let mut m = vec![false; bw * bh];
    let mut xs = Vec::new();
    for j in 0..bh {
        let sy = y0 + (j as f64 + 0.5) * h / bh as f64;
        xs.clear();
        for r in p {
            let n = r.len() / 2;
            if n < 3 {
                continue;
            }
            for k in 0..n {
                let (ax, ay) = (r[k * 2], r[k * 2 + 1]);
                let (bx, by) = (r[(k + 1) % n * 2], r[(k + 1) % n * 2 + 1]);
                if (ay <= sy) != (by <= sy) {
                    xs.push(ax + (sy - ay) * (bx - ax) / (by - ay));
                }
            }
        }
        xs.sort_by(f64::total_cmp);
        for pair in xs.as_chunks::<2>().0 {
            // centres x0 + (i + 0.5) w / bw in [xs[k], xs[k + 1])
            let i0 = ((pair[0] - x0) * bw as f64 / w - 0.5).ceil().max(0.0) as usize;
            let i1 = (((pair[1] - x0) * bw as f64 / w - 0.5).ceil().max(0.0) as usize).min(bw);
            for c in m[j * bw..].iter_mut().take(i1).skip(i0) {
                *c = true;
            }
        }
    }
    m
}

fn douglas_peucker(r: &[f64], a: usize, b: usize, tol2: f64, keep: &mut [bool]) {
    let (ax, ay, bx, by) = (r[a * 2], r[a * 2 + 1], r[b * 2], r[b * 2 + 1]);
    let (dx, dy) = (bx - ax, by - ay);
    let len2 = dx * dx + dy * dy;
    let (mut worst, mut at) = (-1.0, 0);
    for i in a + 1..b {
        let (px, py) = (r[i * 2] - ax, r[i * 2 + 1] - ay);
        let d2 = if len2 <= 0.0 {
            px * px + py * py
        } else {
            let t = ((px * dx + py * dy) / len2).clamp(0.0, 1.0);
            let (ex, ey) = (px - t * dx, py - t * dy);
            ex * ex + ey * ey
        };
        if d2 > worst {
            worst = d2;
            at = i;
        }
    }
    if worst <= tol2 {
        return;
    }
    keep[at] = true;
    douglas_peucker(r, a, at, tol2, keep);
    douglas_peucker(r, at, b, tol2, keep);
}

/// Fewer vertices along nearly straight runs (Douglas-Peucker, tolerance `tol`), per ring.
pub fn simplify(p: &[Ring], tol: f64) -> Polygon {
    let mut out = Vec::new();
    for r in p {
        let n = r.len() / 2;
        if n <= 4 {
            out.push(r.clone());
            continue;
        }
        // split at the vertex farthest from the first, so a closed ring keeps its shape
        let (mut far, mut best) = (0, -1.0);
        for i in 1..n {
            let d = (r[i * 2] - r[0]).hypot(r[i * 2 + 1] - r[1]);
            if d > best {
                best = d;
                far = i;
            }
        }
        let mut closed = r.clone();
        closed.extend([r[0], r[1]]);
        let mut keep = vec![false; n + 1];
        (keep[0], keep[far], keep[n]) = (true, true, true);
        douglas_peucker(&closed, 0, far, tol * tol, &mut keep);
        douglas_peucker(&closed, far, n, tol * tol, &mut keep);
        let s: Ring = (0..n).filter(|&i| keep[i]).flat_map(|i| [r[i * 2], r[i * 2 + 1]]).collect();
        if s.len() >= 6 {
            out.push(s);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The L of the C++'s --selftest-ui stage 113 (concave: its bounding box is not the polygon).
    fn l_shape() -> Polygon {
        vec![vec![20.0, 30.0, 150.0, 30.0, 150.0, 80.0, 70.4, 80.0, 70.4, 170.0, 20.0, 170.0]]
    }

    #[test]
    fn l_shape_area_contains_bounds() {
        let p = l_shape();
        assert!((area(&p) - (130.0 * 50.0 + 50.4 * 90.0)).abs() < 1e-9);
        assert!(!contains(&p, 100.0, 120.0) && contains(&p, 30.0, 120.0) && contains(&p, 100.0, 50.0));
        assert_eq!(bounds(&p), Some([20.0, 30.0, 150.0, 170.0]));
        assert_eq!(bounds(&[]), None);
    }

    #[test]
    fn hole_and_mask() {
        // a 10 x 10 square with a 4 x 4 hole
        let p = vec![vec![0.0, 0.0, 10.0, 0.0, 10.0, 10.0, 0.0, 10.0], vec![3.0, 3.0, 7.0, 3.0, 7.0, 7.0, 3.0, 7.0]];
        assert!((area(&p) - 84.0).abs() < 1e-12);
        let m = mask(&p, 0.0, 0.0, 10.0, 10.0, 10, 10);
        assert_eq!(m.iter().filter(|&&v| v).count(), 84);
        for j in 0..10 {
            for i in 0..10 {
                assert_eq!(m[j * 10 + i], contains(&p, i as f64 + 0.5, j as f64 + 0.5));
            }
        }
    }

    #[test]
    fn mask_matches_contains_on_the_l() {
        let p = l_shape();
        let m = mask(&p, 20.0, 30.0, 130.0, 140.0, 130, 140);
        for j in 0..140 {
            for i in 0..130 {
                assert_eq!(m[j * 130 + i], contains(&p, 20.0 + i as f64 + 0.5, 30.0 + j as f64 + 0.5), "{i} {j}");
            }
        }
    }

    #[test]
    fn densify_transform_simplify() {
        let p = vec![vec![0.0, 0.0, 10.0, 0.0, 10.0, 10.0, 0.0, 10.0]];
        let d = densify(&p, 1.0);
        assert_eq!(d[0].len(), 2 * 40);
        let s = simplify(&d, 0.01);
        assert_eq!(s[0].len(), 8);
        let mut t = d.clone();
        transform(&mut t, |x, y| (x < 5.0).then_some((x + 1.0, y)));
        assert!(t[0].as_chunks::<2>().0.iter().all(|v| v[0] < 6.0));
        transform(&mut t, |_, _| None);
        assert!(t.is_empty());
    }

    #[test]
    fn interior_point_and_line_centroid() {
        // a U (its box's centre in the gap), a square with a hole in its middle, two islands
        let u = vec![vec![0.0, 0.0, 30.0, 0.0, 30.0, 30.0, 20.0, 30.0, 20.0, 10.0, 10.0, 10.0, 10.0, 30.0, 0.0, 30.0]];
        let holed =
            vec![vec![0.0, 0.0, 10.0, 0.0, 10.0, 10.0, 0.0, 10.0], vec![3.0, 3.0, 7.0, 3.0, 7.0, 7.0, 3.0, 7.0]];
        let islands = vec![vec![0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0], vec![5.0, 5.0, 9.0, 5.0, 9.0, 9.0, 5.0, 9.0]];
        for p in [u, holed, islands] {
            let (x, y) = interior_point(&p).unwrap();
            assert!(contains(&p, x, y), "({x}, {y}) outside {p:?}");
        }
        assert_eq!(interior_point(&[]), None);
        // an L of lines: the long leg weighs three times the short one
        let (x, y) = line_centroid(&[vec![0.0, 0.0, 3.0, 0.0], vec![3.0, 0.0, 3.0, 1.0]]).unwrap();
        assert!((x - (1.5 * 3.0 + 3.0) / 4.0).abs() < 1e-12 && (y - 0.5 / 4.0).abs() < 1e-12);
        assert_eq!(line_centroid(&[vec![2.0, 4.0, 2.0, 4.0]]), Some((2.0, 4.0)));
    }
}
