//! `--selftest-ui` stages 270-274: the features' labels and a layer larger than the old drawing budget. Written
//! here, on the active layer's grid (its CRS): `fields.gpkg`, a large polygon, a smaller one whose label would
//! cover the large one's, a tiny one, a line and a point, each with a `name`; `large.gpkg`, 122 500 polygons of 20
//! vertices (2.45 million vertices: the CPU drew 2 million a frame at most, the rest waiting for a closer zoom).
//! The labels from the `name` column (larger first, none over another, none on a feature too small on screen, a
//! line's at its middle, a point's beside it), in a map's export, copied with the style; the large layer read
//! whole, drawn whole by the GPU zoomed out (every part of it has its lines in the frame), the feature under the
//! cursor found through the index.

use std::time::Instant;

use egui::{Id, LayerId, Order, Pos2, Rect};
use gdal::spatial_ref::{AxisMappingStrategy, SpatialRef};
use gdal::vector::{Geometry, LayerAccess, LayerOptions, OGRFieldType};

use super::UiTest;
use crate::app::State;

/// The fields' features in a frame of 200 x 200 units laid over the active layer: the large polygon, the smaller
/// one, the tiny one (x0, y0, x1, y1), the line (shorter than the large one's box: labelled after it), the point.
const BIG: [f64; 4] = [20.0, 20.0, 120.0, 120.0];
const SMALL: [f64; 4] = [75.0, 62.0, 100.0, 87.0];
const TINY: [f64; 4] = [150.0, 20.0, 154.0, 24.0];
const LINE: [f64; 4] = [20.0, 150.0, 100.0, 150.0];
const POINT: [f64; 2] = [160.0, 100.0];
/// The large layer: N x N polygons.
const N: usize = 350;
/// The GPU target of the large layer's check.
const SLOT: i32 = 97;

impl State {
    /// A GeoPackage of `feats` (WKT in active pixels, a name each) in the active layer's CRS.
    fn write_gpkg(&self, path: &std::path::Path, feats: &[(String, String)]) -> Result<(), String> {
        let info = &self.session.as_ref().ok_or("no series")?.info;
        let mut srs = SpatialRef::from_wkt(&info.crs_wkt).map_err(|e| e.to_string())?;
        srs.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
        let _ = std::fs::remove_file(path);
        let drv = gdal::DriverManager::get_driver_by_name("GPKG").map_err(|e| e.to_string())?;
        let mut ds = drv.create_vector_only(path).map_err(|e| e.to_string())?;
        let mut tx = ds.start_transaction().map_err(|e| e.to_string())?;
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let l = tx
            .create_layer(LayerOptions {
                name: &stem,
                srs: Some(&srs),
                ty: gdal::vector::OGRwkbGeometryType::wkbUnknown,
                options: None,
            })
            .map_err(|e| e.to_string())?;
        l.create_defn_fields(&[("name", OGRFieldType::OFTString)]).map_err(|e| e.to_string())?;
        for (wkt, name) in feats {
            let g = Geometry::from_wkt(wkt).map_err(|e| e.to_string())?;
            let mut f = gdal::vector::Feature::new(l.defn()).map_err(|e| e.to_string())?;
            f.set_geometry(g).map_err(|e| e.to_string())?;
            f.set_field_string(0, name).map_err(|e| e.to_string())?;
            f.create(&l).map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(())
    }

    /// The test's two files: the fields (in a frame of 200 units over the active layer) and the large layer.
    fn write_draw_files(&self, dir: &std::path::Path) -> Result<(), String> {
        let info = &self.session.as_ref().ok_or("no series")?.info;
        let s = info.width.min(info.height) as f64 / 200.0;
        let geo = |x: f64, y: f64| -> Result<String, String> {
            let (gx, gy) = info.pixel_to_geo(x, y).ok_or("the active layer has no georeferencing")?;
            Ok(format!("{gx:.6} {gy:.6}"))
        };
        let rect = |b: [f64; 4]| -> Result<String, String> {
            let c = [[b[0], b[1]], [b[2], b[1]], [b[2], b[3]], [b[0], b[3]], [b[0], b[1]]];
            let p: Vec<String> = c.iter().map(|q| geo(q[0] * s, q[1] * s)).collect::<Result<_, _>>()?;
            Ok(format!("POLYGON (({}))", p.join(", ")))
        };
        let feats = vec![
            (rect(BIG)?, "Big field".to_string()),
            (rect(SMALL)?, "Small".to_string()),
            (rect(TINY)?, "Tiny".to_string()),
            (
                format!("LINESTRING ({}, {})", geo(LINE[0] * s, LINE[1] * s)?, geo(LINE[2] * s, LINE[3] * s)?),
                "River".into(),
            ),
            (format!("POINT ({})", geo(POINT[0] * s, POINT[1] * s)?), "Well".into()),
        ];
        self.write_gpkg(&dir.join("fields.gpkg"), &feats)?;
        // the large layer: a 20-gon in each cell of an N x N grid over the whole layer
        let (cw, ch) = (info.width as f64 / N as f64, info.height as f64 / N as f64);
        let mut feats = Vec::with_capacity(N * N);
        for r in 0..N {
            for c in 0..N {
                let (cx, cy) = ((c as f64 + 0.5) * cw, (r as f64 + 0.5) * ch);
                let mut p: Vec<String> = (0..=20)
                    .map(|k| {
                        let a = (k % 20) as f64 / 20.0 * std::f64::consts::TAU;
                        geo(cx + 0.35 * cw * a.cos(), cy + 0.35 * ch * a.sin())
                    })
                    .collect::<Result<_, _>>()?;
                p[20] = p[0].clone();
                feats.push((format!("POLYGON (({}))", p.join(", ")), format!("P{r}-{c}")));
            }
        }
        self.write_gpkg(&dir.join("large.gpkg"), &feats)
    }

    /// The labels placed in a view of `scale` screen points per active pixel centred on (x, y) of the active
    /// layer: (layer, feature, rectangle).
    fn test_labels_at(&mut self, x: f64, y: f64, scale: f64, ctx: &egui::Context) -> Vec<(usize, usize, Rect)> {
        let Some(g) = self.grid() else { return Vec::new() };
        let canvas = Rect::from_min_max(Pos2::ZERO, Pos2::new(800.0, 600.0));
        self.map.canvas = [canvas.width(), canvas.height()];
        self.map.set_view([x / g.fx, y / g.fy], scale * g.fx);
        let layer = LayerId::new(Order::Foreground, Id::new("vector labels test"));
        let painter = ctx.layer_painter(layer).with_clip_rect(canvas);
        let m = &self.map;
        let at = |x: f64, y: f64| m.screen_of(canvas, x / g.fx, y / g.fy);
        let placed = self.vector_labels_placed(&painter, canvas, &at);
        ctx.graphics_mut(|gl| std::mem::take(gl.entry(layer)));
        placed
    }

    pub(super) fn test_vector_draw(
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
            st.since = Some(Instant::now());
        };
        let dir = st.dir.join("vector-draw");
        let layer_of = |s: &State, file: &str| {
            s.vectors.iter().position(|v| std::path::Path::new(&v.data.path).file_name().is_some_and(|f| f == file))
        };
        match stage {
            270 => {
                if self.opening.is_some() || !super::loaded(self.session.as_ref()) {
                    return None;
                }
                let _ = std::fs::create_dir_all(&dir);
                let t = Instant::now();
                if let Err(e) = self.write_draw_files(&dir) {
                    return fail(&format!("the test's vector files: {e}"));
                }
                println!(
                    "    fields.gpkg and large.gpkg ({} polygons) written in {:.1} s",
                    N * N,
                    t.elapsed().as_secs_f64()
                );
                let files = ["fields.gpkg", "large.gpkg"].map(|f| dir.join(f).to_string_lossy().into_owned());
                self.add_vector_files(files.to_vec(), ctx);
                next(st, "the fields and a layer of 2.45 million vertices added");
                None
            }
            271 => {
                // read, placed and drawn by the GPU
                let since = st.since.map_or(0.0, |s| s.elapsed().as_secs_f64());
                let ready = |s: &State| {
                    s.vector_loads.is_empty()
                        && ["fields.gpkg", "large.gpkg"].iter().all(|f| {
                            layer_of(s, f).is_some_and(|i| {
                                let v = &s.vectors[i];
                                v.placed_for != 0 && !v.px.is_empty() && v.gpu.draws(v.placed_for)
                            })
                        })
                };
                if !ready(self) {
                    return if since > 120.0 {
                        fail("the layers should be read, placed and drawn by the GPU")
                    } else {
                        None
                    };
                }
                let fi = layer_of(self, "fields.gpkg")?;
                self.vectors[fi]
                    .labels
                    .set_style(crate::vector_labels::LabelStyle { field: "name".into(), ..Default::default() });
                next(st, "both layers read, placed and drawn by the GPU; labels from the name column asked");
                None
            }
            272 => {
                let fi = layer_of(self, "fields.gpkg")?;
                let since = st.since.map_or(0.0, |s| s.elapsed().as_secs_f64());
                if self.vectors[fi].labels.ready(self.vectors[fi].placed_for).is_none() {
                    if !self.vectors[fi].labels.error.is_empty() {
                        return fail(&format!("the labels: {}", self.vectors[fi].labels.error));
                    }
                    return if since > 60.0 { fail("the labels' column should be read") } else { None };
                }
                let info = self.session.as_ref()?.info.clone();
                let s = info.width.min(info.height) as f64 / 200.0;
                // the large layer hidden for the labels' checks
                let li = layer_of(self, "large.gpkg")?;
                self.vectors[li].visible = false;
                // 2 screen points a frame unit: the large one's label, the line's and the point's; not the smaller
                // one's (over the large one's) nor the tiny one's (8 points: under twice the label's height)
                let placed = self.test_labels_at(100.0 * s, 100.0 * s, 2.0 / s, ctx);
                let names: Vec<usize> = placed.iter().filter(|p| p.0 == fi).map(|p| p.1).collect();
                println!(
                    "    labels placed: {names:?} (0 the large polygon, 1 the smaller, 2 the tiny, 3 the line, 4 the point)"
                );
                if names != vec![0, 3, 4] {
                    return fail("the large polygon's, the line's and the point's labels, in that order");
                }
                let rect_of = |k: usize| placed.iter().find(|p| p.0 == fi && p.1 == k).map(|p| p.2);
                let (river, well) = (rect_of(3)?, rect_of(4)?);
                let g = self.grid()?;
                let canvas = Rect::from_min_max(Pos2::ZERO, Pos2::new(800.0, 600.0));
                let mid = self.map.screen_of(canvas, 60.0 * s / g.fx, 150.0 * s / g.fy);
                let pt = self.map.screen_of(canvas, POINT[0] * s / g.fx, POINT[1] * s / g.fy);
                if (river.center() - mid).length() > 1.0 || well.min.x <= pt.x || !well.y_range().contains(pt.y) {
                    return fail("the line's label at its middle, the point's to its right");
                }
                // zoomed onto the smaller one, the large one's point out of view: its label
                let placed = self.test_labels_at(92.0 * s, 74.5 * s, 40.0 / s, ctx);
                let names: Vec<usize> = placed.iter().filter(|p| p.0 == fi).map(|p| p.1).collect();
                if names != vec![1] {
                    return fail(&format!("zoomed onto the smaller polygon, its label alone (got {names:?})"));
                }
                // in an export: the labels drawn on the image, none with the labels off
                self.test_labels_at(100.0 * s, 100.0 * s, 2.0 / s, ctx);
                let count = |st: &State| {
                    let (w, h) = (800usize, 600usize);
                    let mut px = vec![0u8; w * h * 4];
                    let mut cv = crate::canvas::Canvas { w: w as i32, h: h as i32, px: &mut px };
                    let m = &st.map;
                    st.draw_vector_labels_on(
                        &mut cv,
                        &|x, y| {
                            let p = m.screen_of(canvas, x / g.fx, y / g.fy);
                            (p.x, p.y)
                        },
                        1.0,
                    );
                    px.chunks(4).filter(|p| p[3] > 0).count()
                };
                let on = count(self);
                let style = self.vectors[fi].labels.style.clone();
                self.vectors[fi].labels.set_style(Default::default());
                let off = count(self);
                self.vectors[fi].labels.set_style(style);
                println!("    the export's labels: {on} pixels drawn ({off} without labels)");
                if on < 200 || off != 0 {
                    return fail("an export should draw the labels");
                }
                // copied with the style
                let (from, to) = (self.vectors[fi].id, self.vectors[li].id);
                let copied = self.style_of(crate::contents::Sel::Vector(from))?;
                if self.paste_style(crate::contents::Sel::Vector(to), &copied) != 1
                    || self.vectors[li].labels.style.field != "name"
                {
                    return fail("Copy style should copy the labels");
                }
                self.vectors[li].labels.set_style(Default::default());
                self.vectors[li].visible = true;
                next(
                    st,
                    "labels: larger first, none over another, none too small, at a line's middle, in an export, copied",
                );
                None
            }
            273 => {
                let li = layer_of(self, "large.gpkg")?;
                let v = &self.vectors[li];
                if v.data.truncated || v.data.features.len() != N * N || v.data.vertices < 2_000_000 {
                    return fail("the large layer should be read whole");
                }
                // zoomed out on the whole layer, every other layer hidden: each of 8 x 8 blocks of the frame has its
                // lines (the CPU drew 2 million vertices a frame at most)
                let shown: Vec<bool> = self.layers.iter().map(|l| l.visible).collect();
                let vshown: Vec<bool> = self.vectors.iter().map(|v| v.visible).collect();
                for l in &mut self.layers {
                    l.visible = false;
                }
                for (k, v) in self.vectors.iter_mut().enumerate() {
                    v.visible = k == li;
                }
                let g = self.grid()?;
                let size = [800u32, 600u32];
                let scale = 0.95 * (800.0 / g.ow).min(600.0 / g.oh);
                self.map.canvas = [800.0, 600.0];
                self.map.set_view([g.ow / 2.0, g.oh / 2.0], scale);
                self.map.export_bg = Some([0.0, 0.0, 0.0, 1.0]);
                self.render_map(SLOT, size, [800.0, 600.0], 1.0);
                self.map.export_bg = None;
                let frame = self.gpu.as_ref().and_then(|gpu| gpu.read_target(SLOT));
                for (l, v) in self.layers.iter_mut().zip(shown) {
                    l.visible = v;
                }
                for (x, v) in self.vectors.iter_mut().zip(vshown) {
                    x.visible = v;
                }
                self.map.dirty = true;
                let Some((w, h, px)) = frame else { return fail("the frame should be read back") };
                let canvas = Rect::from_min_max(Pos2::ZERO, Pos2::new(w as f32, h as f32));
                let (a, b) = (self.map.screen_of(canvas, 0.0, 0.0), self.map.screen_of(canvas, g.ow, g.oh));
                let mut empty = 0;
                for by in 0..8 {
                    for bx in 0..8 {
                        let x0 = (a.x + (b.x - a.x) * bx as f32 / 8.0) as usize;
                        let x1 = (a.x + (b.x - a.x) * (bx + 1) as f32 / 8.0) as usize;
                        let y0 = (a.y + (b.y - a.y) * by as f32 / 8.0) as usize;
                        let y1 = (a.y + (b.y - a.y) * (by + 1) as f32 / 8.0) as usize;
                        let lit = (y0..y1.min(h as usize))
                            .flat_map(|y| (x0..x1.min(w as usize)).map(move |x| (x, y)))
                            .filter(|&(x, y)| {
                                let i = (y * w as usize + x) * 4;
                                u32::from(px[i]) + u32::from(px[i + 1]) + u32::from(px[i + 2]) > 60
                            })
                            .count();
                        empty += usize::from(lit * 4 < (x1 - x0) * (y1 - y0));
                    }
                }
                println!(
                    "    zoomed out ({:.3} points a pixel): {empty} of 64 blocks with few lines",
                    self.screen_per_px()
                );
                if empty > 0 {
                    return fail("the large layer should be drawn whole zoomed out");
                }
                // the feature under the cursor: a polygon's centre finds it, a cell's corner none (the fields hidden)
                let fi = layer_of(self, "fields.gpkg")?;
                self.vectors[fi].visible = false;
                let info = self.session.as_ref()?.info.clone();
                let (cw, ch) = (info.width as f64 / N as f64, info.height as f64 / N as f64);
                let t = Instant::now();
                let mut wrong = 0;
                for k in 0..200 {
                    let (r, c) = ((k * 37) % N, (k * 101) % N);
                    let at = self.vector_feature_at((c as f64 + 0.5) * cw, (r as f64 + 0.5) * ch, 1.0);
                    wrong += usize::from(at != Some((li, r * N + c)));
                    let corner = self.vector_feature_at(c as f64 * cw + 0.02 * cw, r as f64 * ch + 0.02 * ch, 0.0);
                    wrong += usize::from(corner.is_some_and(|x| x.0 == li));
                }
                self.vectors[fi].visible = true;
                println!(
                    "    the feature under 400 places: {:.1} us each, {wrong} wrong",
                    t.elapsed().as_secs_f64() * 1e6 / 400.0
                );
                if wrong > 0 {
                    return fail("the feature under the cursor should be the polygon there");
                }
                next(st, "the large layer read whole, drawn whole zoomed out, the feature under the cursor found");
                None
            }
            _ => {
                for f in ["large.gpkg", "fields.gpkg"] {
                    if let Some(i) = layer_of(self, f) {
                        self.remove_vector(i);
                    }
                }
                let _ = std::fs::remove_dir_all(&dir);
                st.stage = 280;
                None
            }
        }
    }
}
