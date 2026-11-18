//! The map PNG's coordinate grid and north arrow (phase 60), drawn on the export's canvas like the scale bar. The grid is
//! of longitude and latitude (the exact PROJ transform for any CRS, so the lines curve where the CRS does) or of the
//! layer's own CRS units (eastings and northings); its lines run at round values (whole degrees, minutes or seconds; 1,
//! 2 or 5 times a power of ten in metres), with their values at the image's edges, tick marks inside it. The north
//! arrow points to true north: it leans by the grid convergence at the centre of the map in a projected CRS.

use gdal::spatial_ref::{AxisMappingStrategy, CoordTransform, SpatialRef};

use crate::canvas::{Canvas, FIGURE_FONT_SIZE, Face, Rgba, rgba};

/// What the grid is of.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Units {
    /// longitude and latitude (WGS 84), whatever the layer's CRS
    LonLat,
    /// the layer's CRS (eastings and northings; degrees in a geographic one)
    Crs,
}

type FromGeo = Box<dyn Fn(f64, f64) -> Option<(f64, f64)>>;

/// The image's pixels and the layer's CRS, in both directions: what a grid needs.
pub struct GeoMap {
    /// the image's size in pixels
    pub size: (f64, f64),
    /// an image pixel's layer CRS coordinates, and back (None: not representable)
    to_geo: Box<dyn Fn(f64, f64) -> (f64, f64)>,
    from_geo: FromGeo,
    to_ll: CoordTransform,
    from_ll: CoordTransform,
    /// the CRS is geographic (its units are degrees)
    pub geographic: bool,
}

impl GeoMap {
    /// A map of an image of `size` pixels whose layer has the CRS `wkt`; `to_geo` and `from_geo` take an image pixel to
    /// the CRS' coordinates and back (the layer's geotransform and the view's).
    pub fn new(
        size: (f64, f64),
        wkt: &str,
        to_geo: impl Fn(f64, f64) -> (f64, f64) + 'static,
        from_geo: impl Fn(f64, f64) -> Option<(f64, f64)> + 'static,
    ) -> Option<GeoMap> {
        let mut src = SpatialRef::from_wkt(wkt).ok()?;
        let mut ll = SpatialRef::from_epsg(4326).ok()?;
        src.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
        ll.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
        Some(GeoMap {
            size,
            to_geo: Box::new(to_geo),
            from_geo: Box::new(from_geo),
            to_ll: CoordTransform::new(&src, &ll).ok()?,
            from_ll: CoordTransform::new(&ll, &src).ok()?,
            geographic: src.is_geographic(),
        })
    }

    fn point(t: &CoordTransform, x: f64, y: f64) -> Option<(f64, f64)> {
        let (mut xs, mut ys) = ([x], [y]);
        t.transform_coords(&mut xs, &mut ys, &mut []).ok()?;
        (xs[0].is_finite() && ys[0].is_finite()).then_some((xs[0], ys[0]))
    }

    /// An image pixel's longitude and latitude.
    pub fn lon_lat(&self, px: f64, py: f64) -> Option<(f64, f64)> {
        let (x, y) = (self.to_geo)(px, py);
        Self::point(&self.to_ll, x, y)
    }

    /// The image pixel of a longitude and latitude.
    pub fn pixel_of_lon_lat(&self, lon: f64, lat: f64) -> Option<(f64, f64)> {
        let (x, y) = Self::point(&self.from_ll, lon, lat)?;
        (self.from_geo)(x, y)
    }

    /// The image pixel of a coordinate of the layer's CRS.
    pub fn pixel_of_crs(&self, x: f64, y: f64) -> Option<(f64, f64)> {
        (self.from_geo)(x, y)
    }

    /// The angle, clockwise from the image's up, of true north at image pixel (px, py), in radians (None: unknown).
    /// 0 in a geographic CRS, the grid convergence in a projected one.
    pub fn north_angle(&self, px: f64, py: f64) -> Option<f64> {
        let (lon, lat) = self.lon_lat(px, py)?;
        // a small step north (south at the pole)
        let dlat = if lat > 89.0 { -0.001 } else { 0.001 };
        let (qx, qy) = self.pixel_of_lon_lat(lon, lat + dlat)?;
        let (dx, dy) = (qx - px, qy - py);
        let (dx, dy) = if dlat < 0.0 { (-dx, -dy) } else { (dx, dy) };
        (dx.hypot(dy) > 0.0).then(|| dx.atan2(-dy))
    }
}

/// The nearest round step at or above `want` (in the grid's units): 1, 2 or 5 times a power of ten; in degrees also the
/// minutes and seconds' round values (15 and 30 as well).
pub fn nice_step(want: f64, degrees: bool) -> f64 {
    if !(want.is_finite() && want > 0.0) {
        return 1.0;
    }
    if degrees {
        let mut c: Vec<f64> = Vec::new();
        for unit in [1.0 / 3600.0, 1.0 / 60.0] {
            c.extend([1.0, 2.0, 5.0, 10.0, 15.0, 30.0].map(|m| m * unit));
        }
        c.extend([1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 45.0, 90.0]);
        if let Some(&s) = c.iter().find(|&&s| s >= want * 0.999) {
            return s;
        }
        return 90.0;
    }
    let p = 10f64.powf(want.log10().floor());
    [1.0, 2.0, 5.0, 10.0].iter().map(|m| m * p).find(|&s| s >= want * 0.999).unwrap_or(10.0 * p)
}

/// A longitude or a latitude as a label: whole degrees where the step is, then minutes, then seconds.
pub fn degrees_label(v: f64, lon: bool, step: f64) -> String {
    let total = (v.abs() * 3600.0).round() as i64;
    let (d, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    let hemi = match (lon, v < 0.0, total == 0) {
        (_, _, true) => "",
        (true, false, _) => "E",
        (true, true, _) => "W",
        (false, false, _) => "N",
        (false, true, _) => "S",
    };
    if step >= 1.0 - 1e-9 {
        format!("{d}\u{b0}{hemi}")
    } else if step >= 1.0 / 60.0 - 1e-9 {
        format!("{d}\u{b0}{m:02}'{hemi}")
    } else {
        format!("{d}\u{b0}{m:02}'{s:02}\"{hemi}")
    }
}

/// A coordinate in the layer's CRS (metres: whole numbers, the step's decimals under 1).
pub fn crs_label(v: f64, step: f64) -> String {
    if step >= 1.0 {
        format!("{}", v.round() as i64)
    } else {
        let decimals = (-step.log10().floor()).max(0.0) as usize;
        format!("{v:.decimals$}")
    }
}

/// One line of the grid in image pixels, and where its value goes.
pub struct GridLine {
    /// a meridian (or a constant easting) as opposed to a parallel
    pub vertical: bool,
    /// its value in the grid's units (the tests read it)
    #[allow(dead_code)]
    pub value: f64,
    pub pts: Vec<(f32, f32)>,
    pub label: String,
}

/// The grid of `units` over the image, lines about `target` across, each a polyline of image pixels.
pub fn grid_lines(g: &GeoMap, units: Units, target: usize) -> Vec<GridLine> {
    let (w, h) = g.size;
    let lonlat = units == Units::LonLat || g.geographic;
    // the extent of the image in the grid's coordinates: its border and an interior grid sampled
    let mut ext = [f64::INFINITY, f64::NEG_INFINITY, f64::INFINITY, f64::NEG_INFINITY];
    let n = 24;
    for i in 0..=n {
        for j in 0..=n {
            let edge = i == 0 || j == 0 || i == n || j == n || (i % 6 == 0 && j % 6 == 0);
            if !edge {
                continue;
            }
            let (px, py) = (w * i as f64 / n as f64, h * j as f64 / n as f64);
            let c = if units == Units::LonLat { g.lon_lat(px, py) } else { Some((g.to_geo)(px, py)) };
            if let Some((x, y)) = c {
                ext = [ext[0].min(x), ext[1].max(x), ext[2].min(y), ext[3].max(y)];
            }
        }
    }
    if !ext.iter().all(|v| v.is_finite()) {
        return Vec::new();
    }
    let to_px = |x: f64, y: f64| -> Option<(f64, f64)> {
        match units {
            Units::LonLat => g.pixel_of_lon_lat(x, y),
            Units::Crs => g.pixel_of_crs(x, y),
        }
    };
    let (sx, sy) = (ext[1] - ext[0], ext[3] - ext[2]);
    let span = sx.max(sy);
    let step = nice_step(span / target.max(2) as f64, lonlat);
    let mut out = Vec::new();
    // a line of constant x (a meridian) and one of constant y, sampled along the other coordinate
    for vertical in [true, false] {
        let (a0, a1, b0, b1) =
            if vertical { (ext[0], ext[1], ext[2], ext[3]) } else { (ext[2], ext[3], ext[0], ext[1]) };
        let pad = (b1 - b0) * 0.02;
        let mut v = (a0 / step).ceil() * step;
        let mut guard = 0;
        while v <= a1 + step * 1e-9 && guard < 200 {
            guard += 1;
            let pts: Vec<(f32, f32)> = (0..=96)
                .filter_map(|k| {
                    let t = b0 - pad + (b1 - b0 + 2.0 * pad) * f64::from(k) / 96.0;
                    let (x, y) = if vertical { (v, t) } else { (t, v) };
                    to_px(x, y).map(|(px, py)| (px as f32, py as f32))
                })
                .collect();
            if pts.len() > 1 {
                let label = if lonlat { degrees_label(v, vertical, step) } else { crs_label(v, step) };
                out.push(GridLine { vertical, value: v, pts, label });
            }
            v += step;
        }
    }
    out
}

/// Where a polyline crosses the horizontal line `y` (the first crossing), or the vertical line `x`.
fn crossing(pts: &[(f32, f32)], at: f32, horizontal: bool) -> Option<(f32, f32)> {
    for p in pts.windows(2) {
        let (a, b) = (p[0], p[1]);
        let (ca, cb) = if horizontal { (a.1, b.1) } else { (a.0, b.0) };
        if (ca - at) * (cb - at) <= 0.0 && ca != cb {
            let t = (at - ca) / (cb - ca);
            return Some((a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t));
        }
    }
    None
}

/// The grid over the canvas: the lines (a dark line under a light one: legible on any map), tick marks at the edges and
/// the values there (meridians at the bottom, parallels at the left), in points x `ps`.
pub fn draw_grid(cv: &mut Canvas, lines: &[GridLine], ps: f32) {
    let fs = FIGURE_FONT_SIZE * ps;
    let (under, over): (Rgba, Rgba) = (rgba(0, 0, 0, 80), rgba(255, 255, 255, 170));
    let flat = |l: &GridLine| -> Vec<f32> { l.pts.iter().flat_map(|p| [p.0, p.1]).collect() };
    for l in lines {
        let pts = flat(l);
        cv.polyline(&pts, under, 2.2 * ps, false);
        cv.polyline(&pts, over, ps, false);
    }
    let (w, h) = (cv.w as f32, cv.h as f32);
    let (left, bottom) = (6.0 * ps, h - 6.0 * ps);
    let tick = 6.0 * ps;
    let box_col = rgba(0, 0, 0, 150);
    let ink = rgba(255, 255, 255, 255);
    let mut last_end = f32::NEG_INFINITY;
    for l in lines.iter().filter(|l| l.vertical) {
        if let Some((x, _)) = crossing(&l.pts, bottom, true).filter(|p| p.0 > 0.0 && p.0 < w) {
            cv.line(x, h, x, h - tick, ink, ps);
            let tw = Canvas::text_width(&l.label, fs, Face::Bitmap);
            let x0 = (x - tw / 2.0).clamp(2.0 * ps, (w - tw - 2.0 * ps).max(2.0 * ps));
            if x0 < last_end + 6.0 * ps {
                continue; // no room beside the last label
            }
            last_end = x0 + tw;
            let y0 = h - tick - fs - 4.0 * ps;
            cv.fill_rect(x0 - 3.0 * ps, y0 - 2.0 * ps, x0 + tw + 3.0 * ps, y0 + fs + 2.0 * ps, box_col);
            cv.text(x0, y0, &l.label, fs, ink, Face::Bitmap);
        }
    }
    let mut last_end = f32::NEG_INFINITY;
    let mut parallels: Vec<(&GridLine, (f32, f32))> =
        lines.iter().filter(|l| !l.vertical).filter_map(|l| crossing(&l.pts, left, false).map(|p| (l, p))).collect();
    parallels.sort_by(|a, b| a.1.1.total_cmp(&b.1.1));
    for (l, (_, y)) in parallels.into_iter().filter(|(_, p)| p.1 > 0.0 && p.1 < h) {
        cv.line(0.0, y, tick, y, ink, ps);
        let tw = Canvas::text_width(&l.label, fs, Face::Bitmap);
        let y0 = (y - fs - 3.0 * ps).clamp(2.0 * ps, (h - fs - 2.0 * ps).max(2.0 * ps));
        if y0 < last_end + 4.0 * ps {
            continue;
        }
        last_end = y0 + fs;
        let x0 = tick + 2.0 * ps;
        cv.fill_rect(x0 - 3.0 * ps, y0 - 2.0 * ps, x0 + tw + 3.0 * ps, y0 + fs + 2.0 * ps, box_col);
        cv.text(x0, y0, &l.label, fs, ink, Face::Bitmap);
    }
}

/// The north arrow, top right, pointing `angle` radians clockwise from up, in points x `ps`: half dark, half white,
/// with an N over it, on a dark box.
pub fn draw_north_arrow(cv: &mut Canvas, angle: f64, ps: f32) {
    let fs = FIGURE_FONT_SIZE * ps;
    let len = 22.0 * ps;
    let (cx, cy) = (cv.w as f32 - 10.0 * ps - 18.0 * ps, 10.0 * ps + fs + 6.0 * ps + len);
    cv.fill_rect(cx - 20.0 * ps, cy - len - fs - 10.0 * ps, cx + 20.0 * ps, cy + len + 5.0 * ps, rgba(0, 0, 0, 140));
    let (s, c) = (angle.sin() as f32, angle.cos() as f32);
    // a point of the arrow's own frame (x right, y up, the tip at (0, len)) in the canvas
    let at = |x: f32, y: f32| (cx + x * c + y * s, cy + 0.0 - (y * c - x * s) + 0.0);
    let tip = at(0.0, len);
    let tail = at(0.0, -len * 0.35);
    let left = at(-len * 0.42, -len * 0.9);
    let right = at(len * 0.42, -len * 0.9);
    let dark = rgba(30, 30, 30, 255);
    let white = rgba(255, 255, 255, 255);
    cv.fill_polygon(&[tip.0, tip.1, left.0, left.1, tail.0, tail.1], dark);
    cv.fill_polygon(&[tip.0, tip.1, right.0, right.1, tail.0, tail.1], white);
    let outline = [tip.0, tip.1, right.0, right.1, tail.0, tail.1, left.0, left.1];
    cv.polyline(&outline, white, 0.8 * ps, true);
    let tw = Canvas::text_width("N", fs, Face::Bitmap);
    let label = at(0.0, len + 3.0 * ps);
    cv.text(label.0 - tw / 2.0, label.1 - fs - 2.0 * ps, "N", fs, white, Face::Bitmap);
}

#[cfg(test)]
mod tests {
    use super::*;

    const WGS84: u32 = 4326;

    /// A north-up raster in `wkt` with its top left at (x0, y0) and pixels of `d`, drawn at `scale` image pixels a
    /// pixel (the map's transform).
    fn map(epsg: u32, x0: f64, y0: f64, d: f64, cols: f64, rows: f64, scale: f64) -> GeoMap {
        let wkt = SpatialRef::from_epsg(epsg).expect("an EPSG code").to_wkt().expect("its WKT");
        GeoMap::new(
            (cols * scale, rows * scale),
            &wkt,
            move |px, py| (x0 + px / scale * d, y0 - py / scale * d),
            move |x, y| Some(((x - x0) / d * scale, (y0 - y) / d * scale)),
        )
        .expect("a CRS")
    }

    #[test]
    fn round_steps() {
        // minutes: 15 min (0.25 degree) is the first round step over 0.18
        assert!((nice_step(0.18, true) - 0.25).abs() < 1e-12);
        assert_eq!(nice_step(3.0, true), 5.0);
        assert_eq!(nice_step(12.0, true), 15.0);
        assert!((nice_step(0.01, true) - 1.0 / 60.0).abs() < 1e-12);
        assert!((nice_step(0.0005, true) - 2.0 / 3600.0).abs() < 1e-12);
        assert_eq!(nice_step(130.0, false), 200.0);
        assert_eq!(nice_step(2000.0, false), 2000.0);
        assert_eq!(nice_step(2100.0, false), 5000.0);
        assert_eq!(degrees_label(-59.5, true, 0.25), "59\u{b0}30'W");
        assert_eq!(degrees_label(-10.0, false, 1.0), "10\u{b0}S");
        assert_eq!(degrees_label(0.0, true, 1.0), "0\u{b0}");
        assert_eq!(degrees_label(12.0 + 30.0 / 60.0 + 15.0 / 3600.0, true, 1.0 / 3600.0), "12\u{b0}30'15\"E");
        assert_eq!(crs_label(523000.0, 1000.0), "523000");
        assert_eq!(crs_label(0.25, 0.05), "0.25");
    }

    #[test]
    fn lon_lat_grid_of_a_geographic_raster() {
        // 1 x 1 degree of EPSG:4326 (lon -60..-59, lat -10..-9) in 100 x 100 pixels drawn at 4 image pixels a pixel
        let g = map(WGS84, -60.0, -9.0, 0.01, 100.0, 100.0, 4.0);
        let lines = grid_lines(&g, Units::LonLat, 5);
        let meridians: Vec<&GridLine> = lines.iter().filter(|l| l.vertical).collect();
        let parallels: Vec<&GridLine> = lines.iter().filter(|l| !l.vertical).collect();
        // a step of 15 minutes: 5 meridians, -60 to -59
        assert_eq!(meridians.len(), 5);
        let at =
            |v: f64| meridians.iter().find(|l| (l.value - v).abs() < 1e-9).unwrap_or_else(|| panic!("meridian {v}"));
        for (v, x) in [(-59.75, 100.0), (-59.5, 200.0), (-59.25, 300.0)] {
            let l = at(v);
            assert!(l.pts.iter().all(|p| (f64::from(p.0) - x).abs() < 0.01), "meridian {v} at x = {x}: {:?}", l.pts[0]);
        }
        assert_eq!(at(-59.5).label, "59\u{b0}30'W");
        // a parallel at -9.75 is three quarters of the way down
        let p = parallels.iter().find(|l| (l.value + 9.75).abs() < 1e-9).expect("parallel -9.75");
        assert!(p.pts.iter().all(|q| (f64::from(q.1) - 300.0).abs() < 0.01), "{:?}", p.pts[0]);
        assert_eq!(p.label, "9\u{b0}45'S");
        // north is up in a geographic CRS
        assert!(g.north_angle(200.0, 200.0).unwrap().abs() < 1e-6);
    }

    /// UTM zone 21 south (EPSG:32721): the central meridian -57 at the easting 500000.
    #[test]
    fn grid_of_a_utm_raster() {
        // 20 x 20 km at 100 m drawn 1 image pixel a pixel: easting 500000..520000, northing 8880000..8900000
        let g = map(32721, 500_000.0, 8_900_000.0, 100.0, 200.0, 200.0, 1.0);
        // layer CRS units: 20 km over 5 lines is 4 km: 5 km is the round step
        let crs = grid_lines(&g, Units::Crs, 5);
        let vs: Vec<f64> = crs.iter().filter(|l| l.vertical).map(|l| l.value).collect();
        assert_eq!(vs, vec![500_000.0, 505_000.0, 510_000.0, 515_000.0, 520_000.0]);
        let e = crs.iter().find(|l| l.vertical && l.value == 510_000.0).unwrap();
        assert!(e.pts.iter().all(|p| (f64::from(p.0) - 100.0).abs() < 0.01));
        assert_eq!(e.label, "510000");
        let n = crs.iter().find(|l| !l.vertical && l.value == 8_890_000.0).unwrap();
        assert!(n.pts.iter().all(|p| (f64::from(p.1) - 100.0).abs() < 0.01));
        // longitude and latitude: the exact transform. The meridian 56 deg 55' W is 5' east of the central meridian: at
        // latitude 10 S one radian of longitude is k0 N cos(phi) = 6279377 m, so 9132 m east, x = 91.3
        let ll = grid_lines(&g, Units::LonLat, 5);
        let m = ll
            .iter()
            .filter(|l| l.vertical)
            .find(|l| (l.value + (56.0 + 55.0 / 60.0)).abs() < 1e-9)
            .expect("the meridian 56 55 W");
        assert_eq!(m.label, "56\u{b0}55'W");
        let mid = m.pts[m.pts.len() / 2];
        assert!((f64::from(mid.0) - 91.3).abs() < 1.0, "x = {}", mid.0);
        // the pixel of the line's point is the longitude's, whatever the latitude (it is a meridian)
        let back = g.lon_lat(f64::from(mid.0), f64::from(mid.1)).unwrap();
        assert!((back.0 + (56.0 + 55.0 / 60.0)).abs() < 1e-4, "{back:?}");
        let p = ll.iter().find(|l| !l.vertical).unwrap();
        assert!(p.pts.len() > 10);
        // the convergence: ~0 on the central meridian, opposite signs either side, about dlon x sin(lat)
        let centre = map(32721, 499_000.0, 8_900_000.0, 100.0, 20.0, 20.0, 1.0);
        assert!(centre.north_angle(10.0, 10.0).unwrap().abs().to_degrees() < 0.01);
        let west = map(32721, 200_000.0, 8_900_000.0, 100.0, 200.0, 200.0, 1.0).north_angle(100.0, 100.0).unwrap();
        let east = map(32721, 800_000.0, 8_900_000.0, 100.0, 200.0, 200.0, 1.0).north_angle(100.0, 100.0).unwrap();
        assert!(east * west < 0.0, "{east} {west}");
        // 295 km west at 10 S: 2.7 degrees of longitude x sin(10 deg) = 0.47 degrees
        for a in [east, west] {
            assert!(a.to_degrees().abs() > 0.40 && a.to_degrees().abs() < 0.55, "{}", a.to_degrees());
        }
    }

    #[test]
    fn the_grid_and_the_arrow_are_drawn() {
        let g = map(WGS84, -60.0, -9.0, 0.01, 100.0, 100.0, 4.0);
        let lines = grid_lines(&g, Units::LonLat, 5);
        let (w, h) = (400usize, 400usize);
        let mut px = vec![0u8; w * h * 4];
        {
            let mut cv = Canvas { w: w as i32, h: h as i32, px: &mut px };
            cv.fill_rect(0.0, 0.0, w as f32, h as f32, rgba(60, 90, 60, 255));
            draw_grid(&mut cv, &lines, 1.0);
            draw_north_arrow(&mut cv, 0.0, 1.0);
        }
        // the meridian at x = 200: light pixels in a column far from the labels, none beside it
        let at = |x: usize, y: usize| px[(y * w + x) * 4];
        let line = at(200, 150).max(at(199, 150)); // the line at x = 200.0 shares two pixel columns
        assert!(line > 100 && at(190, 150) < 90, "{line} {}", at(190, 150));
        // the north arrow's white half, top right
        let white =
            (0..h / 4).flat_map(|y| (w * 3 / 4..w).map(move |x| (x, y))).filter(|&(x, y)| at(x, y) > 240).count();
        assert!(white > 60, "{white}");
    }
}
