//! `--selftest-ui` stages 260-269: the ROI saved as a vector, its vertices edited, the profiles of a line layer.
//! On the active series (A opened if none is): 260 draws an irregular polygon as the ROI and saves it as a
//! GeoPackage (opened as a layer) and a GeoJSON (read back: the same rings and pixels); 261 makes the GeoPackage's
//! polygon the ROI again (Alt + click's path): the same box and pixels; 262-263 edit its vertices with the test's
//! own pointer on the map (a vertex dragged, a new one dragged out of an edge's middle, one removed by a right click
//! and one by Delete, each checked against the polygon expected and its pixels; the last one undone; Esc ends it, no
//! pin dropped); 264 writes a line layer (a polyline across the series and a multi-line partly off it), 265 runs
//! Profiles of every line on it, 266 checks every row of its long and wide tables against the pixels read directly,
//! and the first line's samples against the transect along it; 267 closes what it opened.

use std::cell::RefCell;

use egui::{Event, Pos2, Rect, vec2};
use janus_geo::plane::{self, Polygon};
use janus_geo::vectors::{Placer, load_vector_file};

use super::UiTest;
use crate::app::State;
use crate::roi_edit::Handle;

/// What the stages hand on.
#[derive(Default)]
struct RoiTest {
    dir: std::path::PathBuf,
    rect: [usize; 4],
    mask: Vec<bool>,
    pixels: usize,
    step: usize,
    /// the polygon before an edit, the screen point the pointer is taken to
    before: Polygon,
    target: Pos2,
    pins: usize,
    undo_steps: usize,
    /// frames to wait for the events given
    wait: usize,
    line_layer: Option<u32>,
    folder: String,
    vectors: Vec<u32>,
}

thread_local! {
    static T: RefCell<RoiTest> = RefCell::new(RoiTest::default());
}

fn t<R>(f: impl FnOnce(&mut RoiTest) -> R) -> R {
    T.with_borrow_mut(f)
}

/// The cells of the box whose centres are in the polygon.
fn mask_of(p: &Polygon, r: [usize; 4]) -> Vec<bool> {
    let (w, h) = (r[2] - r[0], r[3] - r[1]);
    plane::mask(p, r[0] as f64, r[1] as f64, w as f64, h as f64, w, h)
}

fn press(pos: Pos2, button: egui::PointerButton, pressed: bool) -> Event {
    Event::PointerButton { pos, button, pressed, modifiers: egui::Modifiers::NONE }
}

fn key(k: egui::Key) -> Event {
    Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }
}

impl State {
    fn file_layer(&self, file: &std::path::Path) -> Option<usize> {
        self.vectors.iter().position(|v| std::path::Path::new(&v.data.path) == file)
    }

    /// The main map's canvas (the largest seen while editing).
    fn edit_canvas(&self) -> Option<Rect> {
        self.roi_edit.rects.iter().map(|r| r.1).max_by(|a, b| a.area().total_cmp(&b.area()))
    }

    fn to_screen(&self, rect: Rect, v: [f64; 2]) -> Option<Pos2> {
        let g = self.grid()?;
        Some(self.map.screen_of(rect, v[0] / g.fx, v[1] / g.fy))
    }

    fn to_pixels(&self, rect: Rect, p: Pos2) -> Option<[f64; 2]> {
        let g = self.grid()?;
        let [x, y] = self.map.image_at(rect, p);
        Some([x * g.fx, y * g.fy])
    }

    /// The ROI now: the polygon expected, its vertices and pixels.
    fn roi_is(&self, want: &Polygon) -> Result<(), String> {
        let got = self.series.roi_poly.clone();
        let n = |p: &Polygon| p.iter().map(|r| r.len() / 2).sum::<usize>();
        if n(&got) != n(want) {
            return Err(format!("{} vertices, {} expected", n(&got), n(want)));
        }
        let worst = got.iter().flatten().zip(want.iter().flatten()).map(|(a, b)| (a - b).abs()).fold(0.0, f64::max);
        if worst > 1e-6 {
            return Err(format!("a vertex {worst:.2e} px from where it should be"));
        }
        let r = self.series.roi_rect.ok_or("no ROI")?;
        let inside = mask_of(want, r).iter().filter(|&&m| m).count();
        if self.roi_pixels() != inside || self.roi_edit.poly != *want {
            return Err(format!("{} pixels, {inside} expected", self.roi_pixels()));
        }
        Ok(())
    }

    pub(super) fn test_roi_profiles(
        &mut self,
        st: &mut UiTest,
        t0: std::time::Instant,
        ctx: &egui::Context,
    ) -> Option<i32> {
        let stage = st.stage;
        let fail = |what: &str| {
            println!("FAIL (stage {stage}): {what}");
            Some(1)
        };
        let next = |st: &mut UiTest, done: &str| {
            println!("[{:5.1} s] {done}", t0.elapsed().as_secs_f64());
            st.stage += 1;
            st.since = Some(std::time::Instant::now());
        };
        match stage {
            260 => {
                // a georeferenced series of several dates, active and read
                if self.opening.is_some() || !self.layer_jobs.queue.is_empty() {
                    return None;
                }
                let ok = self.session.as_ref().is_some_and(|s| s.info.t() > 1 && s.info.geo_transform.is_some());
                if !ok {
                    if st.texts.first().is_none_or(|t| t != "A opened for the ROI") {
                        st.texts = vec!["A opened for the ROI".into()];
                        self.open_inputs(vec!["A".into()], true, ctx);
                    }
                    return None;
                }
                if !super::loaded(self.session.as_ref()) {
                    return None;
                }
                let info = self.session.as_ref()?.info.clone();
                let (w, h) = (info.width as f64, info.height as f64);
                // an irregular pentagon, its vertices off the pixels' corners
                let (cx, cy, r) = (w * 0.41 + 0.3, h * 0.47 + 0.2, w.min(h) * 0.2);
                let ring: Vec<f64> = (0..5)
                    .flat_map(|k| {
                        let a = k as f64 * std::f64::consts::TAU / 5.0 + 0.3;
                        let rr = r * [1.0, 0.8, 1.15, 0.9, 1.05][k];
                        [cx + rr * a.cos(), cy + rr * a.sin()]
                    })
                    .collect();
                self.clear_pins();
                self.set_roi_edit(false);
                if !self.set_roi_polygon(vec![ring]) {
                    return fail("the pentagon should become the ROI");
                }
                let rect = self.series.roi_rect?;
                let mask = mask_of(&self.series.roi_poly, rect);
                let pixels = mask.iter().filter(|&&m| m).count();
                let dir = st.dir.join("roi");
                let _ = std::fs::create_dir_all(&dir);
                let (gpkg, gj) = (dir.join("roi.gpkg"), dir.join("roi.geojson"));
                let saved = self.save_roi_vector(&gpkg.to_string_lossy(), true, ctx);
                if saved != Ok(pixels) {
                    return fail(&format!("the ROI saved as a GeoPackage: {saved:?}, {pixels} pixels expected"));
                }
                if let Err(e) = self.save_roi_vector(&gj.to_string_lossy(), false, ctx) {
                    return fail(&format!("the ROI saved as GeoJSON: {e}"));
                }
                // the GeoJSON read back (OGR): the same rings, the same pixels
                let v = match load_vector_file(&gj.to_string_lossy(), 1_000_000) {
                    Ok(v) => v.into_iter().next()?,
                    Err(e) => return fail(&format!("the GeoJSON read back: {e}")),
                };
                let p = Placer::new(&v, &info.crs_wkt, info.geo_transform).ok()?;
                let back = p.placed(&v.features.first()?.parts);
                let worst = back
                    .iter()
                    .flatten()
                    .zip(self.series.roi_poly.iter().flatten())
                    .map(|(a, b)| (a - b).abs())
                    .fold(0.0, f64::max);
                println!(
                    "    the ROI ({pixels} pixels) saved: GeoPackage and GeoJSON in {}; the GeoJSON's vertices {worst:.1e} px from the ROI's, its fields {:?}",
                    if info.crs_authority.is_empty() { "the layer's CRS" } else { &info.crs_authority },
                    v.features[0].values
                );
                if worst > 1e-6 || mask_of(&back, rect) != mask || v.features[0].values[2] != pixels.to_string() {
                    return fail("the GeoJSON should read back as the ROI, the same pixels");
                }
                // a shapefile too (its rings turned clockwise): the same pixels
                let shp = dir.join("roi.shp");
                if let Err(e) = self.save_roi_vector(&shp.to_string_lossy(), false, ctx) {
                    return fail(&format!("the ROI saved as a shapefile: {e}"));
                }
                let v = match load_vector_file(&shp.to_string_lossy(), 1_000_000) {
                    Ok(v) => v.into_iter().next()?,
                    Err(e) => return fail(&format!("the shapefile read back: {e}")),
                };
                let p = Placer::new(&v, &info.crs_wkt, info.geo_transform).ok()?;
                let back = p.placed(&v.features.first()?.parts);
                if mask_of(&back, rect) != mask || v.features[0].values[2] != pixels.to_string() {
                    return fail("the shapefile should read back as the ROI, the same pixels");
                }
                println!("    and as a shapefile: the same {pixels} pixels, its fields {:?}", v.features[0].values);
                t(|x| *x = RoiTest { dir, rect, mask, pixels, pins: self.series.pins.len(), ..Default::default() });
                next(st, "the ROI saved as a vector (GeoPackage opened as a layer, GeoJSON read back)");
            }
            261 => {
                let gpkg = t(|x| x.dir.join("roi.gpkg"));
                let Some(li) = self.file_layer(&gpkg) else {
                    if self.vector_loads.is_empty() {
                        return fail("the GeoPackage should open as a vector layer");
                    }
                    return None;
                };
                let key = self.vector_place_key();
                if self.vectors[li].placed_for != key || self.vectors[li].px.is_empty() {
                    return None;
                }
                t(|x| x.vectors.push(self.vectors[li].id));
                let d = &self.vectors[li].data;
                let crs = self.session.as_ref()?.info.crs_authority.clone();
                if d.fields != ["name", "layer", "pixels"] || d.areas != 1 || (!crs.is_empty() && d.crs_name != crs) {
                    return fail(&format!("the GeoPackage: {:?}, {} polygons, {}", d.fields, d.areas, d.crs_name));
                }
                // Alt + click on it: the ROI again
                self.clear_roi();
                if !self.feature_as_roi(li, 0) {
                    return fail("the saved polygon should become the ROI");
                }
                let (rect, mask, pixels) = t(|x| (x.rect, x.mask.clone(), x.pixels));
                let same = self.series.roi_rect == Some(rect) && mask_of(&self.series.roi_poly, rect) == mask;
                println!(
                    "    the saved polygon as the ROI: box {:?} ({rect:?} before), {} pixels ({pixels} before)",
                    self.series.roi_rect,
                    self.roi_pixels()
                );
                if !same || self.roi_pixels() != pixels {
                    return fail("the saved polygon should make the same ROI: the same pixels inside");
                }
                next(st, "the saved polygon made the same ROI again");
            }
            262 => {
                // the ROI in view, large; its vertices edited with the test's pointer
                self.set_roi_edit(true);
                if !self.roi_edit.on || self.roi_shape().as_ref() != Some(&self.roi_edit.poly) {
                    return fail("Edit ROI vertices should take the ROI's polygon");
                }
                let (g, b) = (self.grid()?, plane::bounds(&self.roi_edit.poly)?);
                let c = self.map.canvas;
                let s = 0.6 * f64::from(c[0].min(c[1]).max(200.0)) / (b[2] - b[0]).max(b[3] - b[1]);
                self.map.set_view([(b[0] + b[2]) / 2.0 / g.fx, (b[1] + b[3]) / 2.0 / g.fy], s * g.fx);
                self.roi_edit.test_pointer = true;
                self.record_undo(); // what came before: not counted below
                t(|x| {
                    x.step = 0;
                    x.undo_steps = self.undo.past.len();
                });
                next(st, "Edit ROI vertices on, the ROI in view");
            }
            263 => {
                let rect = self.edit_canvas()?;
                // events given now reach the map at the next frame, their effect is seen at the one after
                if t(|x| x.wait.checked_sub(1).map(|w| x.wait = w)).is_some() {
                    return None;
                }
                let step = t(|x| x.step);
                let poly = self.roi_edit.poly.clone();
                let mut ev: Vec<Event> = Vec::new();
                let mut push = |e: Vec<Event>| ev.extend(e);
                let primary = egui::PointerButton::Primary;
                match step {
                    // a vertex dragged
                    0 => {
                        let p0 = self.to_screen(rect, [poly[0][0], poly[0][1]])?;
                        let target = p0 + vec2(30.0, -20.0);
                        t(|x| (x.before, x.target) = (poly.clone(), target));
                        push(vec![Event::PointerMoved(p0)]);
                    }
                    1 => {
                        if self.roi_edit.hover.map(|h| h.1) != Some(Handle::Vertex(0, 0)) {
                            return fail("the vertex under the pointer should be its handle");
                        }
                        let p0 = self.to_screen(rect, [poly[0][0], poly[0][1]])?;
                        push(vec![press(p0, primary, true)]);
                    }
                    2 => {
                        let p0 = self.to_screen(rect, [poly[0][0], poly[0][1]])?;
                        push(vec![Event::PointerMoved(p0 + vec2(15.0, -10.0))]);
                    }
                    3 => push(vec![Event::PointerMoved(t(|x| x.target))]),
                    4 => push(vec![press(t(|x| x.target), primary, false)]),
                    5 => {
                        let mut want = t(|x| x.before.clone());
                        let at = self.to_pixels(rect, t(|x| x.target))?;
                        crate::roi_edit::move_vertex(&mut want, 0, 0, at);
                        if let Err(e) = self.roi_is(&want) {
                            return fail(&format!("a vertex dragged: {e}"));
                        }
                        println!("    a vertex dragged: the ROI's, {} pixels", self.roi_pixels());
                        self.record_undo();
                        // a new vertex out of the middle of the edge from vertex 1 to 2
                        let m =
                            self.to_screen(rect, [(poly[0][2] + poly[0][4]) / 2.0, (poly[0][3] + poly[0][5]) / 2.0])?;
                        let c = self.to_screen(rect, {
                            let b = plane::bounds(&poly)?;
                            [(b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0]
                        })?;
                        let out = (m - c).normalized() * 25.0;
                        t(|x| (x.before, x.target) = (poly.clone(), m + out));
                        push(vec![Event::PointerMoved(m)]);
                    }
                    6 => {
                        if self.roi_edit.hover.map(|h| h.1) != Some(Handle::Middle(0, 1)) {
                            return fail("the edge's middle under the pointer should be its handle");
                        }
                        let m =
                            self.to_screen(rect, [(poly[0][2] + poly[0][4]) / 2.0, (poly[0][3] + poly[0][5]) / 2.0])?;
                        push(vec![press(m, primary, true)]);
                    }
                    7 => push(vec![Event::PointerMoved(t(|x| x.target))]),
                    8 => push(vec![press(t(|x| x.target), primary, false)]),
                    9 => {
                        let mut want = t(|x| x.before.clone());
                        let at = self.to_pixels(rect, t(|x| x.target))?;
                        crate::roi_edit::insert_vertex(&mut want, 0, 1, at);
                        if let Err(e) = self.roi_is(&want) {
                            return fail(&format!("a vertex dragged out of an edge's middle: {e}"));
                        }
                        println!("    a vertex inserted: 6 vertices, {} pixels", self.roi_pixels());
                        self.record_undo();
                        // vertex 3 removed by a right click
                        let p = self.to_screen(rect, [poly[0][6], poly[0][7]])?;
                        t(|x| (x.before, x.target) = (poly.clone(), p));
                        push(vec![Event::PointerMoved(p)]);
                    }
                    10 => push(vec![press(t(|x| x.target), egui::PointerButton::Secondary, true)]),
                    11 => push(vec![press(t(|x| x.target), egui::PointerButton::Secondary, false)]),
                    12 => {
                        let mut want = t(|x| x.before.clone());
                        crate::roi_edit::remove_vertex(&mut want, 0, 3);
                        if let Err(e) = self.roi_is(&want) {
                            return fail(&format!("a vertex removed by a right click: {e}"));
                        }
                        self.record_undo();
                        // vertex 0 removed by Delete
                        let p = self.to_screen(rect, [poly[0][0], poly[0][1]])?;
                        t(|x| x.before = poly.clone());
                        push(vec![Event::PointerMoved(p)]);
                    }
                    13 => push(vec![key(egui::Key::Delete)]),
                    14 => {
                        let mut want = t(|x| x.before.clone());
                        crate::roi_edit::remove_vertex(&mut want, 0, 0);
                        if let Err(e) = self.roi_is(&want) {
                            return fail(&format!("a vertex removed by Delete: {e}"));
                        }
                        println!(
                            "    two vertices removed (right click, Delete): 4 vertices, {} pixels",
                            self.roi_pixels()
                        );
                        self.record_undo();
                        let steps = self.undo.past.len() - t(|x| x.undo_steps);
                        if steps != 4 || self.can_undo() != Some("ROI") {
                            return fail(&format!("each edit should be a step of Undo ({steps} steps)"));
                        }
                        self.undo(ctx);
                    }
                    15 => {
                        if self.undo.busy() {
                            return None;
                        }
                    }
                    16 => {
                        // the Delete undone: the polygon before it, followed by the editing
                        let want = t(|x| x.before.clone());
                        if let Err(e) = self.roi_is(&want) {
                            return fail(&format!("Undo: {e}"));
                        }
                        push(vec![key(egui::Key::Escape)]);
                    }
                    _ => {
                        if self.roi_edit.on {
                            return fail("Esc should end the editing");
                        }
                        if self.series.pins.len() != t(|x| x.pins) {
                            return fail("editing the vertices should drop no pin");
                        }
                        self.roi_edit.test_pointer = false;
                        println!("    Undo gave back the vertex removed by Delete; Esc ended the editing");
                        next(st, "the ROI's vertices edited with the pointer (moved, inserted, removed, undone)");
                        return None;
                    }
                }
                let waits = !ev.is_empty();
                self.roi_edit.events.extend(ev);
                t(|x| {
                    x.step += 1;
                    x.wait = if waits { 2 } else { 0 };
                });
            }
            264 => {
                // a line layer in the series' CRS: a polyline across it, and a multi-line partly off it
                self.roi_edit.test_pointer = false;
                let info = self.session.as_ref()?.info.clone();
                let (w, h) = (info.width as f64, info.height as f64);
                let geo = |x: f64, y: f64| info.pixel_to_geo(x, y).unwrap_or_default();
                let lines: [Vec<Vec<(f64, f64)>>; 2] = [
                    vec![vec![
                        (w * 0.05 + 0.3, h * 0.1 + 0.7),
                        (w * 0.5 + 0.2, h * 0.6 + 0.1),
                        (w * 0.9 + 0.4, h * 0.3 + 0.6),
                    ]],
                    vec![
                        vec![(w * 0.2 + 0.5, h * 0.9 + 0.5), (w * 0.7 + 0.5, h * 0.85 + 0.5)],
                        vec![(-w * 0.1, h * 0.3 + 0.4), (w * 0.15 + 0.6, h * 0.35 + 0.2)],
                    ],
                ];
                let path = t(|x| x.dir.join("lines.gpkg"));
                let r = (|| -> Result<(), String> {
                    use gdal::vector::{FieldDefn, Geometry, LayerAccess, LayerOptions, OGRFieldType};
                    let _ = std::fs::remove_file(&path);
                    let drv = gdal::DriverManager::get_driver_by_name("GPKG").map_err(|e| e.to_string())?;
                    let mut ds = drv.create_vector_only(&path).map_err(|e| e.to_string())?;
                    let mut srs = gdal::spatial_ref::SpatialRef::from_wkt(&info.crs_wkt).ok();
                    if let Some(s) = &mut srs {
                        s.set_axis_mapping_strategy(gdal::spatial_ref::AxisMappingStrategy::TraditionalGisOrder);
                    }
                    let l = ds
                        .create_layer(LayerOptions {
                            name: "lines",
                            srs: srs.as_ref(),
                            ty: gdal::vector::OGRwkbGeometryType::wkbMultiLineString,
                            options: None,
                        })
                        .map_err(|e| e.to_string())?;
                    FieldDefn::new("name", OGRFieldType::OFTString)
                        .and_then(|d| d.add_to_layer(&l))
                        .map_err(|e| e.to_string())?;
                    for (k, parts) in lines.iter().enumerate() {
                        let text: Vec<String> = parts
                            .iter()
                            .map(|p| {
                                let v: Vec<String> = p
                                    .iter()
                                    .map(|&(x, y)| {
                                        let (gx, gy) = geo(x, y);
                                        format!("{gx} {gy}")
                                    })
                                    .collect();
                                format!("({})", v.join(", "))
                            })
                            .collect();
                        let g = Geometry::from_wkt(&format!("MULTILINESTRING ({})", text.join(", ")))
                            .map_err(|e| e.to_string())?;
                        let mut f = gdal::vector::Feature::new(l.defn()).map_err(|e| e.to_string())?;
                        f.set_geometry(g).map_err(|e| e.to_string())?;
                        f.set_field_string(0, &format!("line {}", k + 1)).map_err(|e| e.to_string())?;
                        f.create(&l).map_err(|e| e.to_string())?;
                    }
                    Ok(())
                })();
                if let Err(e) = r {
                    return fail(&format!("the line layer could not be written: {e}"));
                }
                self.add_vector_files(vec![path.to_string_lossy().into_owned()], ctx);
                next(st, "a line layer written (a polyline, a multi-line partly off the series)");
            }
            265 => {
                let path = t(|x| x.dir.join("lines.gpkg"));
                let Some(li) = self.file_layer(&path) else {
                    if self.vector_loads.is_empty() {
                        return fail("the line layer should open");
                    }
                    return None;
                };
                let id = self.vectors[li].id;
                t(|x| {
                    x.line_layer = Some(id);
                    x.vectors.push(id);
                });
                self.open_profiles(id);
                (self.profiles.fields, self.profiles.wide, self.profiles.all_dates) = (vec![0], true, true);
                self.profiles.points = 2;
                self.profiles.threads = Some(2);
                let Some(folder) = self.profiles_run(ctx) else {
                    return fail(&format!("Profiles of every line should start: {}", self.profiles.status));
                };
                t(|x| x.folder = folder);
                next(st, "Profiles of every line run on the line layer");
            }
            266 => {
                let Some((status, _)) = &self.profiles.last else { return fail("no profiles' run") };
                let (state, error, msg) = {
                    let s = status.lock().unwrap();
                    (s.state, s.error.clone(), s.message.clone())
                };
                match state {
                    crate::zeit::JobState::Running => return None,
                    crate::zeit::JobState::Done => {}
                    _ => return fail(&format!("the profiles' task: {error}")),
                }
                let folder = std::path::PathBuf::from(t(|x| x.folder.clone()));
                let long = std::fs::read_to_string(folder.join("profiles.csv")).unwrap_or_default();
                let wide = std::fs::read_to_string(folder.join("profiles_wide.csv")).unwrap_or_default();
                let info = self.session.as_ref()?.info.clone();
                let rows: Vec<Vec<&str>> = long.lines().map(|l| l.split(',').collect()).collect();
                let head = rows.first().cloned().unwrap_or_default();
                if head.len() != 11 || head[0] != "fid" || head[1] != "name" || head[9] != "date" {
                    return fail(&format!("the long table's columns: {head:?}"));
                }
                // every value against the pixel read directly
                let date: std::collections::HashMap<&str, usize> =
                    info.layers.iter().enumerate().map(|(k, l)| (l.label.as_str(), k)).collect();
                let (mut checked, mut differ, mut lines) = (0usize, 0usize, std::collections::BTreeSet::new());
                let mut first: Vec<(i64, i64)> = Vec::new();
                for r in &rows[1..] {
                    let (Ok(col), Ok(row), Some(&k)) = (r[7].parse::<usize>(), r[8].parse::<usize>(), date.get(r[9]))
                    else {
                        return fail(&format!("a row of the long table: {r:?}"));
                    };
                    lines.insert((r[0], r[2]));
                    if r[0] == "1" && r[2] == "0" && k == 0 {
                        first.push((col as i64, row as i64));
                    }
                    let v = janus_cube::with_thread_reader(&info, |rd| rd.read_pixel(k, col, row)).unwrap_or(f32::NAN);
                    let got: f32 = if r[10].is_empty() { f32::NAN } else { r[10].parse().unwrap_or(-1.0) };
                    let same = (v.is_nan() && got.is_nan())
                        || (f64::from(v) - f64::from(got)).abs() <= 1e-6 * f64::from(v).abs().max(1.0);
                    differ += usize::from(!same);
                    checked += 1;
                }
                let samples = (rows.len() - 1) / info.t().max(1);
                let wide_rows = wide.lines().count().saturating_sub(1);
                println!(
                    "    {msg}; {checked} values of {} lines checked against the pixels read directly: {differ} differ; the wide table {wide_rows} rows",
                    lines.len()
                );
                if differ > 0 || checked == 0 || lines.len() != 3 || wide_rows != samples {
                    return fail("every profile value should be its pixel's, three lines, a wide row per sample");
                }
                // the samples as points (a shapefile): a point per wide row, at its x and y, its values
                let pts = match load_vector_file(&folder.join("profiles_points.shp").to_string_lossy(), 10_000_000) {
                    Ok(v) => v.into_iter().next()?,
                    Err(e) => return fail(&format!("the profiles' points: {e}")),
                };
                let mut bad = 0;
                for (f, row) in pts.features.iter().zip(wide.lines().skip(1)) {
                    let c: Vec<&str> = row.split(',').collect();
                    let (x, y): (f64, f64) = (c[5].parse().unwrap_or(f64::NAN), c[6].parse().unwrap_or(f64::NAN));
                    let near = |a: f64, b: f64| (a - b).abs() <= 1e-6 * b.abs().max(1.0);
                    let at = f.parts.first().is_some_and(|q| near(q[0], x) && near(q[1], y));
                    let values = c[9..].iter().zip(&f.values[f.values.len() + 9 - c.len()..]).all(|(a, b)| {
                        a.is_empty() && b.is_empty()
                            || a.parse::<f64>().ok().zip(b.parse::<f64>().ok()).is_some_and(|(p, q)| near(q, p))
                    });
                    bad += usize::from(!at || !values);
                }
                println!(
                    "    the samples as points (a shapefile): {} points, {bad} unlike the wide rows",
                    pts.features.len()
                );
                if pts.features.len() != wide_rows || bad > 0 {
                    return fail("a point per sample, at its place, with its values");
                }
                // the first line's samples: the transect's along it
                let li = self.vector_index(t(|x| x.line_layer)?)?;
                if !self.feature_as_transect(li, 0) {
                    return fail("the first line should become the transect");
                }
                let tr = &self.transect;
                let along: Vec<(i64, i64)> =
                    (0..tr.n).filter(|&i| tr.px[i] >= 0).map(|i| (tr.px[i], tr.py[i])).collect();
                println!(
                    "    the first line: {} samples, the transect along it {} (the same pixels: {})",
                    first.len(),
                    along.len(),
                    first == along
                );
                if tr.n < 1024 && first != along {
                    return fail("the first line's samples should be the transect's");
                }
                next(st, "the profiles checked against direct reads and the transect");
            }
            _ => {
                // what the stages opened, closed
                self.clear_transect();
                self.set_roi_edit(false);
                self.clear_roi();
                for id in t(|x| std::mem::take(&mut x.vectors)) {
                    if let Some(i) = self.vector_index(id) {
                        self.remove_vector(i);
                    }
                }
                self.profiles.show = false;
                next(st, "the ROI and profiles' layers closed");
                st.stage = 270;
            }
        }
        None
    }
}
