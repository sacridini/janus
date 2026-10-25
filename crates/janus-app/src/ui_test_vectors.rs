//! `--selftest-ui` stages 120-127 (src/app_vectors.cpp, selfTestVectors): B active, A and F (UTM 22S) as layers.
//! Vector files written here: points in lon/lat (GeoJSON, one far away), a polygon with a hole and a line in UTM
//! 22S (GeoPackage), points in a CSV without a CRS. Their places on B's grid, picking, a polygon as the ROI, the
//! line as the transect, the series extracted (wide over every layer, long for the polygon) against the files; the
//! drawing order (points moved under the series are covered by them); the interpretation: keys as typed, a label
//! and its date, the labels saved, resumed and exported (GeoPackage, CSV); placed again on a thread when another
//! grid is made active.
//! (The C++'s PNGs of the vectors come with the exports.)

use gdal::spatial_ref::{AxisMappingStrategy, CoordTransform, SpatialRef};
use gdal::vector::{Geometry, LayerAccess, LayerOptions, OGRFieldType};

use super::{UiTest, loaded};
use crate::app::State;
use crate::exports::{ExportState, write_text_file};

/// B's pixels: points (the last far away, at 0 N 0 E), the polygon's shell and hole, the line.
const PTS: [[f64; 2]; 4] = [[40.5, 50.5], [200.25, 120.75], [280.5, 10.5], [60.5, 150.5]];
const SHELL: [f64; 8] = [100.0, 40.0, 180.0, 40.0, 180.0, 110.0, 100.0, 110.0];
const HOLE: [f64; 8] = [120.0, 60.0, 150.0, 60.0, 150.0, 90.0, 120.0, 90.0];
const LINE: [f64; 4] = [20.0, 180.0, 90.0, 185.0];

/// The GPU target of the drawing order's check.
const ORDER_SLOT: i32 = 96;

fn read_csv(path: &str) -> Vec<Vec<String>> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim_end_matches('\r').split(',').map(String::from).collect())
        .collect()
}

fn srs(epsg: u32) -> Result<SpatialRef, String> {
    let mut s = SpatialRef::from_epsg(epsg).map_err(|e| e.to_string())?;
    s.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    Ok(s)
}

fn key(ctx: &egui::Context, key: egui::Key, modifiers: egui::Modifiers) {
    ctx.input_mut(|i| {
        i.events.push(egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers })
    });
}

impl State {
    fn vector_layer_of(&self, file: &str) -> Option<usize> {
        self.vectors.iter().position(|v| std::path::Path::new(&v.data.path).file_name().is_some_and(|f| f == file))
    }

    /// The test's files: points in lon/lat (GeoJSON) and the same ones in a CSV without a CRS; a polygon with a
    /// hole and a line in UTM 22S (GeoPackage).
    fn write_vector_files(&self, dir: &std::path::Path, pts: &mut Vec<[f64; 2]>) -> Result<(), String> {
        let b = &self.session.as_ref().ok_or("no series")?.info;
        let (utm23, utm22, wgs) = (srs(32723)?, srs(32722)?, srs(4326)?);
        let to_wgs = CoordTransform::new(&utm23, &wgs).map_err(|e| e.to_string())?;
        let to22 = CoordTransform::new(&utm23, &utm22).map_err(|e| e.to_string())?;
        let place = |t: &CoordTransform, px: f64, py: f64| -> Result<(f64, f64), String> {
            let (x, y) = b.pixel_to_geo(px, py).ok_or("B has no georeferencing")?;
            let (mut xs, mut ys, mut zs) = ([x], [y], [0.0]);
            t.transform_coords(&mut xs, &mut ys, &mut zs).map_err(|e| e.to_string())?;
            Ok((xs[0], ys[0]))
        };
        let (mut gj, mut csv) =
            (String::from("{\"type\": \"FeatureCollection\", \"features\": [\n"), String::from("lon,lat,class\n"));
        pts.clear();
        for i in 0..5 {
            let q = PTS.get(i); // the fifth point at 0 N 0 E
            let (lon, lat) = match q {
                Some(q) => place(&to_wgs, q[0], q[1])?,
                None => (0.0, 0.0),
            };
            if let Some(&q) = q {
                pts.push(q);
                csv += &format!("{lon:.10},{lat:.10},c{i}\n");
            }
            gj += &format!(
                "{}{{\"type\": \"Feature\", \"properties\": {{\"plot\": {}, \"name\": \"p{i}\"}}, \"geometry\": \
                 {{\"type\": \"Point\", \"coordinates\": [{lon:.10}, {lat:.10}]}}}}\n",
                if i > 0 { "," } else { "" },
                101 + i
            );
        }
        gj += "]}\n";
        write_text_file(&dir.join("plots.geojson").to_string_lossy(), &gj)?;
        write_text_file(&dir.join("samples.csv").to_string_lossy(), &csv)?;
        let ring = |v: &[f64]| -> Result<String, String> {
            let p: Vec<String> = v
                .as_chunks::<2>()
                .0
                .iter()
                .map(|q| place(&to22, q[0], q[1]).map(|(x, y)| format!("{x:.6} {y:.6}")))
                .collect::<Result<_, _>>()?;
            Ok(p.join(", "))
        };
        let close = |r: String| {
            let first = r.split(", ").next().unwrap_or("").to_string();
            format!("({r}, {first})")
        };
        let poly = format!("POLYGON ({}, {})", close(ring(&SHELL)?), close(ring(&HOLE)?));
        let line = format!("LINESTRING ({})", ring(&LINE)?);
        let drv = gdal::DriverManager::get_driver_by_name("GPKG").map_err(|e| e.to_string())?;
        let mut ds = drv.create_vector_only(dir.join("parcels.gpkg")).map_err(|e| e.to_string())?;
        let l = ds
            .create_layer(LayerOptions {
                name: "parcels",
                srs: Some(&utm22),
                ty: gdal::vector::OGRwkbGeometryType::wkbUnknown,
                options: None,
            })
            .map_err(|e| e.to_string())?;
        l.create_defn_fields(&[("use", OGRFieldType::OFTString)]).map_err(|e| e.to_string())?;
        for (wkt, use_) in [(poly, "pasture"), (line, "road")] {
            let g = Geometry::from_wkt(&wkt).map_err(|e| e.to_string())?;
            let mut f = gdal::vector::Feature::new(l.defn()).map_err(|e| e.to_string())?;
            f.set_geometry(g).map_err(|e| e.to_string())?;
            f.set_field_string(0, use_).map_err(|e| e.to_string())?;
            f.create(&l).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub(super) fn test_vectors(&mut self, st: &mut UiTest, t0: std::time::Instant, ctx: &egui::Context) -> Option<i32> {
        let fail = |what: &str| {
            println!("FAIL: {what}");
            Some(1)
        };
        let next = |st: &mut UiTest, done: &str| {
            println!("[{:5.1} s] {done}", t0.elapsed().as_secs_f64());
            st.stage += 1;
            st.since = Some(std::time::Instant::now());
        };
        let file = |st: &UiTest, name: &str| st.dir.join(name).to_string_lossy().into_owned();
        match st.stage {
            120 => {
                if self.layers.len() != 3 || self.active != 0 || !(0..3).all(|i| loaded(self.layer_session(i))) {
                    return fail("B should be active, with A and F");
                }
                let stamp =
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
                st.dir = std::env::temp_dir().join(format!("janus-selftest-vectors-{stamp}"));
                let _ = std::fs::create_dir_all(&st.dir);
                let mut pts = Vec::new();
                if let Err(e) = self.write_vector_files(&st.dir.clone(), &mut pts) {
                    return fail(&format!("the vector files cannot be written: {e}"));
                }
                st.pts = pts;
                let files = ["plots.geojson", "parcels.gpkg", "samples.csv"].map(|f| file(st, f)).to_vec();
                self.open_inputs(files, true, ctx); // vectors only
                next(st, "vector files written (GeoJSON in lon/lat, GeoPackage in UTM 22S, CSV) and added");
            }
            121 => {
                let (Some(p), Some(g), Some(c)) = (
                    self.vector_layer_of("plots.geojson"),
                    self.vector_layer_of("parcels.gpkg"),
                    self.vector_layer_of("samples.csv"),
                ) else {
                    return None;
                };
                if !self.vector_loads.is_empty() || [p, g, c].iter().any(|&i| self.vectors[i].placed_for == 0) {
                    return None;
                }
                if self.opening.is_some() || self.layers.len() != 3 {
                    return fail("vector files should not open as rasters");
                }
                let (pv, gv, cv) = (&self.vectors[p], &self.vectors[g], &self.vectors[c]);
                println!(
                    "    {}: {}, {}; {}: {}, {}; {}: {}, {}{}",
                    pv.name,
                    pv.data.kind_text(),
                    pv.data.crs_name,
                    gv.name,
                    gv.data.kind_text(),
                    gv.data.crs_name,
                    cv.name,
                    cv.data.kind_text(),
                    cv.data.crs_name,
                    if cv.data.lon_lat_assumed { " (assumed)" } else { "" }
                );
                if pv.data.points != 5
                    || gv.data.areas != 1
                    || gv.data.lines != 1
                    || cv.data.points != 4
                    || !cv.data.lon_lat_assumed
                    || gv.data.crs_name != "EPSG:32722"
                {
                    return fail("the vector files should read as 5 points, a polygon and a line, 4 points");
                }
                if pv.data.fields != ["plot", "name"] {
                    return fail("the GeoJSON's fields");
                }
                // their geometries alone: the fields' values are read when asked
                if [pv, gv, cv].iter().any(|v| v.table_ready() || v.values(0).is_some()) {
                    return fail("vector layers should open without their fields' values");
                }
                let mut worst = 0f64;
                for (i, q) in st.pts.iter().enumerate() {
                    let (a, b) = (&pv.px[i][0], &cv.px[i][0]);
                    worst = worst.max((a[0] - q[0]).hypot(a[1] - q[1])).max((b[0] - q[0]).hypot(b[1] - q[1]));
                }
                let gp = &gv.px[0];
                for k in 0..4 {
                    worst = worst
                        .max((gp[0][k * 2] - SHELL[k * 2]).hypot(gp[0][k * 2 + 1] - SHELL[k * 2 + 1]))
                        .max((gp[1][k * 2] - HOLE[k * 2]).hypot(gp[1][k * 2 + 1] - HOLE[k * 2 + 1]));
                }
                println!(
                    "    placed on B: max {worst:.2e} px from where they were written (lon/lat and UTM 22S to UTM 23S)"
                );
                if worst > 1e-4 {
                    return fail("vector features should be placed where they were written");
                }
                let far = pv.boxes[4]; // 0 N 0 E in UTM 23S: thousands of km away, or not transformed
                if !(far[0] > far[2] || far[0].abs() > 1e5 || far[1].abs() > 1e5) {
                    return fail("the point at 0 N 0 E should be far off B");
                }
                // picking: a point, inside the polygon, in its hole, on the line
                let csv_on_top = self.vector_feature_at(40.6, 50.4, 6.0).is_some_and(|(l, _)| l == p.max(c));
                self.vectors[c].visible = false; // its points are the GeoJSON's: the one drawn on top is picked
                let on_point = self.vector_feature_at(40.6, 50.4, 6.0) == Some((p, 0));
                let in_area = self.vector_feature_at(110.0, 50.0, 6.0) == Some((g, 0));
                let in_hole = self.vector_feature_at(135.0, 75.0, 6.0).is_some();
                let on_line = self.vector_feature_at(55.0, 182.6, 6.0) == Some((g, 1));
                self.vectors[c].visible = true;
                println!(
                    "    picked: the top point {csv_on_top}, a point {on_point}, the polygon {in_area}, its hole {in_hole}, the line {on_line}"
                );
                if !csv_on_top || !on_point || !in_area || in_hole || !on_line {
                    return fail("picking: the top layer's point, a point, the polygon, not its hole, the line");
                }
                if !self.feature_as_roi(g, 0) || self.series.roi_poly.len() != 2 {
                    return fail("the polygon with its hole should become the ROI");
                }
                // the line (Alt + click on it): the transect along it, written in UTM 22S
                if self.feature_as_roi(g, 1) || !self.feature_as_transect(g, 1) {
                    return fail("the line should become the transect, not the ROI");
                }
                let line_len = (LINE[2] - LINE[0]).hypot(LINE[3] - LINE[1]) * 30.0;
                let tr = &self.transect;
                let (a, b) = (tr.pts[0], *tr.pts.last()?);
                println!(
                    "    transect along the line: {} vertices, ({:.3}, {:.3}) - ({:.3}, {:.3}), {:.1} {} ({line_len:.1} m drawn), {} samples",
                    tr.pts.len(),
                    a[0],
                    a[1],
                    b[0],
                    b[1],
                    tr.length,
                    tr.unit,
                    tr.n
                );
                if !tr.on
                    || (a[0] - LINE[0]).hypot(a[1] - LINE[1]) > 1e-3
                    || (b[0] - LINE[2]).hypot(b[1] - LINE[3]) > 1e-3
                    || (tr.length - line_len).abs() > 0.01
                    || tr.unit != "m"
                {
                    return fail("the transect should follow the line from end to end");
                }
                self.clear_transect();
                // the parcels' attribute table shown (read on a thread), the third plot's fields read alone (as for
                // its tooltip)
                let gid = self.vectors[g].id;
                self.open_attribute_table(gid);
                self.vectors[p].ask_feature(2);
                next(st, "placed on B, picked; the polygon (with its hole) as the ROI, the line as the transect");
            }
            122 => {
                let (p, g) = (self.vector_layer_of("plots.geojson")?, self.vector_layer_of("parcels.gpkg")?);
                // the PNGs and the drawing order's check need the vector layers on the GPU
                if !self.series.roi.complete()
                    || !self.vectors.iter().all(|v| v.px.is_empty() || v.gpu.draws(v.placed_for))
                    || !self.vectors[g].table_ready()
                    || self.vectors[p].values(2).is_none()
                {
                    return None;
                }
                let rows: Vec<_> = (0..2).map(|i| self.vectors[g].row(i).map(<[String]>::to_vec)).collect();
                let plot = self.vectors[p].values(2).map(<[String]>::to_vec);
                println!("    the parcels' attribute table {rows:?}; the third plot's fields read alone {plot:?}");
                let s = |v: &[&str]| Some(v.iter().map(|x| x.to_string()).collect::<Vec<_>>());
                if rows != [s(&["pasture"]), s(&["road"])]
                    || plot != s(&["103", "p2"])
                    || self.vectors[p].table_ready()
                    || self.layout_report.attributes.is_none()
                {
                    return fail("the parcels' table (in its panel) and the third plot's fields, read when asked");
                }
                self.attr_table.show = false;
                let inside = self.series.roi.data.as_ref()?.mask.iter().filter(|&&m| m).count();
                if inside != 80 * 70 - 30 * 30 {
                    return fail("the ROI of the polygon should be its pixels less the hole's");
                }
                println!("    ROI: {inside} pixels (the 80 x 70 shell less the 30 x 30 hole)");
                let a = self.extract_series(p, true, true, file(st, "plots_series.csv"), ctx)?;
                let b = self.extract_series(g, false, false, file(st, "parcels_series.csv"), ctx)?;
                st.jobs = vec![a, b];
                let o =
                    crate::export_map::PngOptions { marks: true, label: false, legend: false, ..Default::default() };
                let Some(png) = self.export_png(file(st, "vectors.png"), o, ctx) else {
                    return fail("the PNG did not start");
                };
                st.jobs.push(png);
                // and without "Pins, ROI and vector layers"
                let o = crate::export_map::PngOptions { marks: false, ..o };
                let Some(png) = self.export_png(file(st, "vectors_off.png"), o, ctx) else {
                    return fail("the PNG did not start");
                };
                st.jobs.push(png);
                next(
                    st,
                    "series extracted (points: every layer, wide; the polygon: long) and PNGs with and without the vectors",
                );
            }
            123 => {
                for j in &st.jobs {
                    j.wait();
                    if j.state() != ExportState::Done {
                        println!("    {}: {}", j.title, j.error());
                        return fail("an extraction failed");
                    }
                }
                let (b, a, f) = (self.layer_session(0)?, self.layer_session(1)?, self.layer_session(2)?);
                let (bi, ai) = (b.info.clone(), a.info.clone());
                let (t_n, ta, tf) = (bi.t(), ai.t(), f.info.t());
                // wide: fid, plot, name, lon, lat, then B's dates, A's, F's
                let w = read_csv(&file(st, "plots_series.csv"));
                if w.len() != 6 || w[0].len() != 5 + t_n + ta + tf {
                    return fail("the wide table's shape");
                }
                if w[0][1] != "plot" || w[0][5] != format!("{} {}", self.layers[0].name, bi.layers[0].label) {
                    return fail("the wide table's header");
                }
                let (mut rb, mut ra) =
                    (janus_cube::CubeReader::new(bi.clone()), janus_cube::CubeReader::new(ai.clone()));
                let close = |got: &str, v: f32| {
                    v.is_nan() == got.is_empty()
                        && (got.is_empty()
                            || got
                                .parse::<f64>()
                                .is_ok_and(|g| (g - f64::from(v)).abs() <= 1e-6 * (1.0 + f64::from(v).abs())))
                };
                let mut compared = 0;
                for (i, q) in PTS.iter().enumerate() {
                    let (x, y) = (q[0] as usize, q[1] as usize);
                    for t in 0..t_n {
                        let v = rb.read_pixel(t, x, y).unwrap_or(f32::NAN);
                        let got = &w[i + 1][5 + t];
                        if !close(got, v) {
                            return fail("a point's series on B differs from the file");
                        }
                        compared += usize::from(!got.is_empty());
                    }
                    for t in 0..ta {
                        // A = B + 100 px: the second point is off A
                        let v = if x + 100 < ai.width {
                            ra.read_pixel(t, x + 100, y).unwrap_or(f32::NAN)
                        } else {
                            f32::NAN
                        };
                        if !close(&w[i + 1][5 + t_n + t], v) {
                            return fail("a point's series on A differs from the file");
                        }
                    }
                }
                if w[5].iter().skip(5).any(|v| !v.is_empty()) {
                    return fail("the point far away should have no values");
                }
                // long: one row per date, the polygon's mean (= the ROI's) and its pixels; the line left out
                let l = read_csv(&file(st, "parcels_series.csv"));
                if l.len() != 1 + t_n || l[0].last().map(String::as_str) != Some("pixels") {
                    return fail("the long table's shape");
                }
                let roi = self.series.roi.data.clone()?;
                let mut worst = 0f64;
                for t in 0..t_n {
                    let row = &l[1 + t];
                    let got = &row[l[0].len() - 2];
                    let e = roi.per_t[t].get().filter(|s| s.n > 0).map_or(f32::NAN, |s| s.mean);
                    if e.is_nan() != got.is_empty() {
                        return fail("the polygon's mean: no data where the ROI has some");
                    }
                    if let Ok(g) = got.parse::<f64>() {
                        worst = worst.max((g - f64::from(e)).abs());
                    }
                    if row.last().map(String::as_str) != Some("4700") {
                        return fail("the polygon's pixels should be 4700");
                    }
                }
                println!(
                    "    wide: {compared} values of B compared, A's and the far point's gaps empty; long: |mean - ROI| {worst:.2e}"
                );
                if worst > 1e-4 {
                    return fail("the polygon's mean should be the ROI's");
                }
                // the PNGs: a point in the GeoJSON's colour (the CSV's points are the GeoJSON's: the one on top
                // shows), none without the vector layers
                {
                    let ppp = ctx.pixels_per_point();
                    let g = self.grid()?;
                    let rect = egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(self.map.canvas[0].floor(), self.map.canvas[1].floor()),
                    );
                    let q = self.map.screen_of(rect, PTS[2][0] / g.fx, PTS[2][1] / g.fy);
                    let at = |name: &str| -> Option<[i32; 3]> {
                        let ds = gdal::Dataset::open(file(st, name)).ok()?;
                        let (w, h) = ds.raster_size();
                        let (x, y) = (((q.x * ppp) as usize).min(w - 1), ((q.y * ppp) as usize).min(h - 1));
                        Some([1, 2, 3].map(|b| {
                            ds.rasterband(b)
                                .and_then(|r| r.read_as::<u8>((x as isize, y as isize), (1, 1), (1, 1), None))
                                .map_or(0, |v| i32::from(v.data()[0]))
                        }))
                    };
                    let (Some(on), Some(off)) = (at("vectors.png"), at("vectors_off.png")) else {
                        return fail("the PNGs cannot be opened");
                    };
                    let (pi, ci) = (self.vector_layer_of("plots.geojson")?, self.vector_layer_of("samples.csv")?);
                    let e = self.vectors[pi.max(ci)].color;
                    let e = [e.r(), e.g(), e.b()].map(i32::from);
                    let near = |c: [i32; 3]| (0..3).all(|k| (c[k] - e[k]).abs() <= 40);
                    println!(
                        "    PNG at the third point: rgb{on:?}, without the vector layers rgb{off:?}, its layer's colour rgb{e:?}"
                    );
                    if !near(on) {
                        return fail("the PNG should show the point in its layer's colour");
                    }
                    if near(off) {
                        return fail("the PNG without the vector layers should not show the point");
                    }
                }
                // the drawing order: the points' layers moved under the series are covered by B where it has data,
                // moved back to the top they show again
                {
                    let t = self.display.t;
                    let Some(q) = PTS[..3]
                        .iter()
                        .find(|q| rb.read_pixel(t, q[0] as usize, q[1] as usize).is_some_and(|v| !v.is_nan()))
                    else {
                        return fail("B should have data at one of the points");
                    };
                    let size = [self.map.canvas[0].floor() as u32, self.map.canvas[1].floor() as u32];
                    let draw = |s: &mut State| -> Option<[i32; 3]> {
                        s.render_map(ORDER_SLOT, size, [size[0] as f32, size[1] as f32], 1.0);
                        let (w, h, px) = s.gpu.as_ref()?.read_target(ORDER_SLOT)?;
                        let g = s.grid()?;
                        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w as f32, h as f32));
                        let p = s.map.screen_of(rect, q[0] / g.fx, q[1] / g.fy);
                        let at =
                            ((p.y as usize).min(h as usize - 1) * w as usize + (p.x as usize).min(w as usize - 1)) * 4;
                        Some([0, 1, 2].map(|k| i32::from(px[at + k])))
                    };
                    let points = [self.vector_layer_of("plots.geojson")?, self.vector_layer_of("samples.csv")?]
                        .map(|i| self.vectors[i].id);
                    let place = |s: &State, id: u32| {
                        let i = s.vector_index(id)?;
                        s.draw_order().iter().position(|it| *it == crate::order::Item::Vector(i))
                    };
                    let on_top = draw(self)?;
                    for id in points {
                        let from = place(self, id)?;
                        self.move_in_order(from, 0);
                    }
                    let under = draw(self)?;
                    let bottom_ok = (0..2).all(|k| {
                        matches!(self.draw_order()[k], crate::order::Item::Vector(i) if points.contains(&self.vectors[i].id))
                    });
                    for id in points {
                        let i = self.vector_index(id)?;
                        self.vectors[i].visible = false;
                    }
                    let hidden = draw(self)?;
                    for id in points {
                        let i = self.vector_index(id)?;
                        self.vectors[i].visible = true;
                        let top = self.draw_order().len() - 1;
                        let from = place(self, id)?;
                        self.move_in_order(from, top);
                    }
                    let back = draw(self)?;
                    if let Some(g) = self.gpu.as_mut() {
                        g.release_target(ORDER_SLOT);
                    }
                    let same = |a: [i32; 3], b: [i32; 3]| (0..3).all(|k| (a[k] - b[k]).abs() <= 2);
                    println!(
                        "    drawing order at ({}, {}): on top rgb{on_top:?}, under the series rgb{under:?}, hidden rgb{hidden:?}, back on top rgb{back:?}",
                        q[0], q[1]
                    );
                    if !bottom_ok || self.active != 0 || self.layers.len() != 3 {
                        return fail("the points' layers should go to the bottom, the series staying as they were");
                    }
                    if !same(under, hidden) || same(on_top, hidden) || !same(back, on_top) {
                        return fail("a vector layer under a series should be covered by it, and show on top");
                    }
                }
                // interpretation of the points: the first one shown, its pin, the map on it
                let p = self.vector_layer_of("plots.geojson")?;
                let id = self.vectors[p].id;
                self.start_interpretation(id);
                if self.labels.current != Some(0) || self.labels.pin.is_none() {
                    return fail("the interpretation should start at the first point, with a pin");
                }
                let g = self.grid()?;
                let a = self.vectors[p].anchor[0];
                let c = self.map.center;
                if (c[0] * g.fx - a[0]).abs() > 1e-3
                    || (c[1] * g.fy - a[1]).abs() > 1e-3
                    || self.screen_per_px() < f64::from(self.labels.zoom) - 1e-6
                {
                    return fail("the map should be centred on the point, zoomed in");
                }
                key(ctx, egui::Key::N, egui::Modifiers::NONE); // as typed: N, then 2, then D
                self.open_extract(id); // its window drawn once, then closed
                st.pin = self.labels.pin;
                st.frames = 0;
                next(st, "tables match the files; interpretation started on the first point");
            }
            124 => {
                st.frames += 1;
                if st.frames == 1 {
                    if self.labels.current != Some(1) {
                        return fail("N should go to the next point");
                    }
                    let pin = self.series.pins.iter().find(|q| Some(q.id) == self.labels.pin);
                    // its number freed and taken again: a pin of that number only on the new point
                    let old_gone = self.series.pins.iter().all(|q| Some(q.id) != st.pin || (q.x == 200 && q.y == 120));
                    if pin.is_none_or(|q| q.x != 200 || q.y != 120) || !old_gone {
                        return fail("the pin should move to the point");
                    }
                    if !self.extract.open {
                        return fail("the Extract series window should open");
                    }
                    self.extract.open = false;
                    key(ctx, egui::Key::Num2, egui::Modifiers::NONE);
                    return None;
                }
                if st.frames == 2 {
                    if self.labels.entries[1].label != self.labels.classes[1] {
                        return fail("2 should give the second label");
                    }
                    self.set_date(3);
                    key(ctx, egui::Key::D, egui::Modifiers::NONE);
                    return None;
                }
                let date = self.session.as_ref()?.info.layers[3].label.clone();
                if self.labels.entries[1].date != date {
                    return fail("D should set the date of the change");
                }
                self.labels.notes = "edge, cloud in 2003".into();
                self.labels.entries[1].notes = self.labels.notes.clone();
                self.save_labels();
                st.reveal = self.labels_file();
                // resumed: stop, start again (from the file in the app data)
                let (id, pin) = (self.labels.vector?, self.labels.pin);
                self.stop_interpretation();
                if self.series.pins.iter().any(|q| Some(q.id) == pin) {
                    return fail("stopping should remove its pin");
                }
                self.start_interpretation(id);
                let e = &self.labels.entries[1];
                if e.label != self.labels.classes[1]
                    || e.notes != "edge, cloud in 2003"
                    || self.labels.current != Some(0)
                {
                    return fail("the labels should be resumed (and the first unlabelled point shown)");
                }
                let a = self.export_labels(file(st, "labels.gpkg"), ctx)?;
                let b = self.export_labels(file(st, "labels.csv"), ctx)?;
                st.jobs = vec![a, b];
                next(st, "N, 2 and D as typed; the labels saved and resumed; exported");
            }
            125 => {
                for j in &st.jobs {
                    j.wait();
                    if j.state() != ExportState::Done {
                        println!("    {}: {}", j.title, j.error());
                        return fail("the labels' export failed");
                    }
                }
                // GeoPackage: the points with their fields and the three new ones
                let date = self.session.as_ref()?.info.layers[3].label.clone();
                let class = self.labels.classes[1].clone();
                let ok = match janus_geo::vectors::load_vector_file(&file(st, "labels.gpkg"), 1_000_000) {
                    Ok(v) => {
                        let d = &v[0];
                        let col = |n: &str| d.fields.iter().position(|f| f == n);
                        let (Some(l), Some(c), Some(n), Some(p)) =
                            (col("label"), col("change_date"), col("notes"), col("plot"))
                        else {
                            return fail(
                                "the GeoPackage of labels should have the fields label, change_date, notes, plot",
                            );
                        };
                        d.features.len() == 5
                            && d.features.iter().enumerate().all(|(k, f)| {
                                let v = &f.values;
                                let labels = if k == 1 {
                                    v[l] == class && v[c] == date && v[n] == "edge, cloud in 2003"
                                } else {
                                    v[l].is_empty()
                                };
                                labels && v[p] == (101 + k).to_string() && !f.parts.is_empty()
                            })
                    }
                    Err(_) => false,
                };
                if !ok {
                    return fail("the GeoPackage of labels should hold the points, their fields and the labels");
                }
                let c = read_csv(&file(st, "labels.csv"));
                let n = c.first().map_or(0, Vec::len);
                if c.len() != 6
                    || c[0].last().map(String::as_str) != Some("notes")
                    || c[2].get(n.wrapping_sub(3)) != Some(&class)
                    || c[2][3].is_empty()
                {
                    return fail("the CSV of labels should hold the label and lon/lat");
                }
                // a polygon in the interpretation: it becomes the ROI
                let g = self.vector_layer_of("parcels.gpkg")?;
                self.start_interpretation(self.vectors[g].id);
                if self.labels.current != Some(0) || self.series.roi_poly.len() != 2 || self.labels.pin.is_some() {
                    return fail("a polygon should be shown as the ROI");
                }
                self.stop_interpretation();
                self.labels.show = false;
                if let Some(r) = st.reveal.take() {
                    let _ = std::fs::remove_file(r);
                }
                self.clear_roi();
                next(
                    st,
                    "labels exported (GeoPackage with the geometries, CSV with lon/lat); a polygon interpreted as the ROI",
                );
                st.frames = 0;
            }
            126 => {
                // placing in the background: A made active (its grid 100 px from B's), the layers placed on a
                // thread while the frames go on, not drawn meanwhile
                let p = self.vector_layer_of("plots.geojson")?;
                if st.frames == 0 {
                    st.pts = self.vectors[p].anchor.iter().map(|a| [a[0], a[1]]).collect();
                    self.set_active(1, ctx);
                    let t = std::time::Instant::now();
                    self.pump_vectors(ctx);
                    let ms = t.elapsed().as_secs_f64() * 1000.0;
                    let v = &self.vectors[p];
                    println!("    A made active: the frame's placing took {ms:.2} ms, the layers placed on a thread");
                    if v.placing.is_none() || !v.px.is_empty() || v.placed_for != 0 {
                        return fail("the layers should be placed again on a thread, not drawn meanwhile");
                    }
                    st.frames = 1;
                    return None;
                }
                let key = self.vector_place_key();
                if self.vectors.iter().any(|v| v.placing.is_some() || v.placed_for != key) {
                    return None;
                }
                let a = &self.vectors[p].anchor;
                let worst = st
                    .pts
                    .iter()
                    .zip(a)
                    .filter(|(b, _)| b[0].is_finite() && b[0].abs() < 1e5)
                    .map(|(b, a)| (a[0] - b[0] - 100.0).hypot(a[1] - b[1]))
                    .fold(0f64, f64::max);
                println!("    placed on A: the points 100 px further than on B (max {worst:.2e} px off)");
                if worst > 1e-6 || a.len() != st.pts.len() {
                    return fail("the points placed on A should be B's 100 px further");
                }
                self.set_active(0, ctx);
                next(st, "placed again on a thread when another grid is made active, where it should be");
            }
            127 => {
                let key = self.vector_place_key();
                if self.vectors.iter().any(|v| v.placing.is_some() || v.placed_for != key) {
                    return None;
                }
                let p = self.vector_layer_of("plots.geojson")?;
                let back =
                    st.pts.iter().zip(&self.vectors[p].anchor).all(|(b, a)| b == a || (b[0].is_nan() && a[0].is_nan()));
                if !back {
                    return fail("back on B, the points should be where they were");
                }
                while !self.vectors.is_empty() {
                    self.remove_vector(0);
                }
                let _ = std::fs::remove_dir_all(&st.dir);
                next(st, "back on B: placed where they were");
                st.stage = 130; // the animation export, then validation on D
                return None;
            }
            _ => {}
        }
        None
    }
}
