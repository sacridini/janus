//! `--measure-vectors IN VEC...` (src/app_vectors.cpp, measureVectorsStep): a series and vector files over it, in
//! the window: reading, placing, drawing (the whole series in view and zoomed in; the shapes and their
//! tessellation, as ImGui tessellates while drawing), picking and the series extracted at every feature.

use std::sync::Arc;
use std::time::Instant;

use egui::{Id, LayerId, Order, Pos2, Rect};

use crate::app::State;
use crate::exports::ExportJob;

#[derive(Default)]
pub struct MeasureVectors {
    stage: usize,
    /// the exit code, once decided (the window takes another frame to close)
    result: Option<i32>,
    jobs: Vec<Arc<ExportJob>>,
    dir: std::path::PathBuf,
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

impl State {
    pub(crate) fn measure_vectors_step(&mut self, ctx: &egui::Context) -> Option<i32> {
        ctx.request_repaint();
        if self.measure_vectors.result.is_some() {
            return self.measure_vectors.result;
        }
        let mut st = std::mem::take(&mut self.measure_vectors);
        let r = self.measure_vectors_stage(&mut st, ctx);
        st.result = r;
        self.measure_vectors = st;
        r
    }

    fn measure_vectors_stage(&mut self, st: &mut MeasureVectors, ctx: &egui::Context) -> Option<i32> {
        match st.stage {
            0 => {
                self.basemap.ui.on = false;
                let s = self.session.as_ref()?;
                if self.opening.is_some() || !s.s.overview.complete() || s.stats.is_none() {
                    return None;
                }
                let files = self.opts.measure_vector_files.clone();
                self.add_vector_files(files, ctx);
                st.stage = 1;
            }
            1 => {
                if !self.vector_loads.is_empty()
                    || self.vectors.is_empty()
                    || self.vectors.iter().any(|v| v.placed_for == 0)
                {
                    return None;
                }
                let info = self.session.as_ref()?.info.clone();
                println!("series: {} x {} px, {} dates ({})", info.width, info.height, info.t(), info.description);
                for i in 0..self.vectors.len() {
                    let mut place = f64::INFINITY;
                    for _ in 0..3 {
                        let t = Instant::now();
                        self.place_vector(i);
                        place = place.min(ms(t));
                    }
                    let v = &self.vectors[i];
                    println!(
                        "{}: {}, {} vertices, {}; read in {:.0} ms, placed on the series' grid in {place:.1} ms",
                        v.name,
                        v.data.kind_text(),
                        v.data.vertices,
                        v.data.crs_name,
                        v.data.load_ms
                    );
                }
                // drawing: every layer, the whole series in view, then 8 points per pixel at its centre
                let canvas = Rect::from_min_max(Pos2::ZERO, Pos2::new(1600.0, 900.0));
                self.map.canvas = [canvas.width(), canvas.height()];
                let g = self.grid()?;
                let layer = LayerId::new(Order::Foreground, Id::new("measure vectors"));
                for zoom in 0..2 {
                    if zoom == 0 {
                        let s = 0.98 * (1600.0 / g.ow).min(900.0 / g.oh);
                        self.map.set_view([g.ow / 2.0, g.oh / 2.0], s);
                    } else {
                        self.map.set_view([g.ow / 2.0, g.oh / 2.0], 8.0 * g.fx);
                    }
                    let (mut best_draw, mut best_all, mut vertices) = (f64::INFINITY, f64::INFINITY, 0);
                    for _ in 0..5 {
                        let painter = ctx.layer_painter(layer).with_clip_rect(canvas);
                        let m = &self.map;
                        let at = |x: f64, y: f64| m.screen_of(canvas, x / g.fx, y / g.fy);
                        let t = Instant::now();
                        self.draw_vector_marks(&painter, canvas, &at);
                        let drawn = ms(t);
                        let shapes =
                            ctx.graphics_mut(|gl| std::mem::take(gl.entry(layer)).all_entries().cloned().collect());
                        let prims = ctx.tessellate(shapes, 1.0);
                        best_draw = best_draw.min(drawn);
                        best_all = best_all.min(ms(t));
                        vertices = prims
                            .iter()
                            .map(|p| match &p.primitive {
                                egui::epaint::Primitive::Mesh(m) => m.vertices.len(),
                                _ => 0,
                            })
                            .sum::<usize>();
                    }
                    println!(
                        "draw, {} ({:.2} points per pixel): {best_all:.2} ms per frame ({best_draw:.2} ms the shapes, the rest \
                         their tessellation), {vertices} vertices",
                        if zoom == 1 { "zoomed in" } else { "whole series" },
                        self.screen_per_px()
                    );
                }
                // the GPU's drawing: the layers built (levels, pieces) and uploaded, then a frame drawn and finished
                let t = Instant::now();
                while self.vectors.iter().any(|v| !v.gpu.draws(v.placed_for)) {
                    self.pump_vector_gpu(ctx);
                    if ms(t) > 600_000.0 {
                        println!("GPU: not built in 10 minutes");
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                for v in &self.vectors {
                    if let Some(g) = &v.gpu.ready {
                        println!(
                            "{}: built for the GPU in {:.0} ms, {:.1} MB on the GPU; points of each level: {:?}",
                            v.name,
                            g.build_ms,
                            g.bytes as f64 / 1048576.0,
                            g.level_points
                        );
                    }
                }
                let size = [1600, 900];
                for zoom in 0..3 {
                    let k = match zoom {
                        0 => 0.98 * (1600.0 / g.ow).min(900.0 / g.oh),
                        1 => 8.0 * 0.98 * (1600.0 / g.ow).min(900.0 / g.oh),
                        _ => 8.0 * g.fx,
                    };
                    self.map.set_view([g.ow / 2.0, g.oh / 2.0], k);
                    let mut best = f64::INFINITY;
                    for _ in 0..5 {
                        self.map.dirty = true;
                        let t = Instant::now();
                        self.render_map(0, size, [1600.0, 900.0], 1.0);
                        if let Some(gpu) = self.gpu.as_ref() {
                            gpu.finish();
                        }
                        best = best.min(ms(t));
                    }
                    println!(
                        "GPU draw, {} ({:.3} points per pixel): {best:.2} ms per frame (series and vectors, finished)",
                        ["whole series", "8x closer", "zoomed in"][zoom],
                        self.screen_per_px()
                    );
                }
                // JANUS_MEASURE_PNG: the whole series' frame saved there
                if let Ok(path) = std::env::var("JANUS_MEASURE_PNG") {
                    self.map.set_view([g.ow / 2.0, g.oh / 2.0], 0.98 * (1600.0 / g.ow).min(900.0 / g.oh));
                    self.map.dirty = true;
                    self.render_map(0, size, [1600.0, 900.0], 1.0);
                    if let Some((w, h, px)) = self.gpu.as_ref().and_then(|gpu| gpu.read_target(0)) {
                        let _ = image::save_buffer(&path, &px, w, h, image::ColorType::Rgba8);
                        println!("saved {path}");
                    }
                    // and as close as the map goes, at the first feature's first vertex
                    let at = self.vectors[0].px.iter().flatten().find(|r| r.len() >= 2).map(|r| [r[0], r[1]]);
                    if let Some(at) = at {
                        self.map.set_view([at[0] / g.fx, at[1] / g.fy], crate::map::MAX_ZOOM * g.fx);
                        self.map.dirty = true;
                        self.render_map(0, size, [1600.0, 900.0], 1.0);
                        if let Some((w, h, px)) = self.gpu.as_ref().and_then(|gpu| gpu.read_target(0)) {
                            let close = path.replace(".png", "_close.png");
                            let _ = image::save_buffer(&close, &px, w, h, image::ColorType::Rgba8);
                            println!("saved {close} ({:.1} points per pixel)", self.screen_per_px());
                        }
                    }
                }
                let s = 0.98 * (1600.0 / g.ow).min(900.0 / g.oh);
                self.map.set_view([g.ow / 2.0, g.oh / 2.0], s);
                // picking: the feature under 2000 places
                let mut seed: u32 = 12345;
                let mut rnd = || {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    f64::from(seed) / 4294967296.0
                };
                let t = Instant::now();
                let mut hits = 0;
                for _ in 0..2000 {
                    let (x, y) = (rnd() * info.width as f64, rnd() * info.height as f64);
                    hits += usize::from(self.vector_feature_at(x, y, 6.0).is_some());
                }
                println!("picking: {:.1} us per place ({hits} of 2000 on a feature)", ms(t) * 1000.0 / 2000.0);
                // the series of every feature, each layer to its own table
                st.dir = std::env::temp_dir().join("janus-measure-vectors");
                let _ = std::fs::create_dir_all(&st.dir);
                for i in 0..self.vectors.len() {
                    let path = st.dir.join(format!("{}.csv", self.vectors[i].id)).to_string_lossy().into_owned();
                    st.jobs.extend(self.extract_series(i, false, true, path, ctx));
                }
                st.stage = 2;
            }
            _ => {
                for j in &st.jobs {
                    j.wait();
                    let kb = std::fs::metadata(&j.path).map_or(0.0, |m| m.len() as f64 / 1024.0);
                    println!("{}: {:.2} s, {kb:.0} KB {}", j.title, j.seconds(), j.error());
                }
                let _ = std::fs::remove_dir_all(&st.dir);
                return Some(0);
            }
        }
        None
    }
}
