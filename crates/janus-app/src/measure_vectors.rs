//! `--measure-vectors IN VEC...` (src/app_vectors.cpp, measureVectorsStep): a series and vector files over it, in
//! the window: reading, placing, the time until every layer is drawn whole (the first frame that draws all of it),
//! drawing (the whole series in view and zoomed in; the CPU's shapes and their tessellation, as ImGui tessellates
//! while drawing; the GPU's frames, p50 and p95 over frames panning), the labels, picking, the process' memory and
//! the series extracted at every feature (small layers). JANUS_MEASURE_PNG: frames saved there (the layers alone,
//! on black); JANUS_MEASURE_JSON: the numbers as JSON (tools/bench_vector_draw.py).

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
    /// when the files were added; read and placed (ms after); frames meanwhile
    t_open: Option<Instant>,
    read_ms: f64,
    timeline: Vec<(f64, f64, &'static str)>,
    placed_ms: f64,
    frames: usize,
    /// the process' memory before the files were added (MB)
    mem_before: f64,
    json: serde_json::Map<String, serde_json::Value>,
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

/// The process' private memory and its peak (MB).
#[cfg(windows)]
pub(crate) fn process_memory_mb() -> (f64, f64) {
    #[repr(C)]
    #[derive(Default)]
    struct Counters {
        cb: u32,
        page_faults: u32,
        peak_working_set: usize,
        working_set: usize,
        quota_peak_paged: usize,
        quota_paged: usize,
        quota_peak_non_paged: usize,
        quota_non_paged: usize,
        pagefile: usize,
        peak_pagefile: usize,
        private: usize,
    }
    unsafe extern "system" {
        fn GetCurrentProcess() -> isize;
        fn K32GetProcessMemoryInfo(process: isize, counters: *mut Counters, cb: u32) -> i32;
    }
    let mut c = Counters { cb: size_of::<Counters>() as u32, ..Default::default() };
    let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) } != 0;
    let mb = |b: usize| b as f64 / 1048576.0;
    if ok { (mb(c.private), mb(c.peak_pagefile)) } else { (0.0, 0.0) }
}

#[cfg(not(windows))]
pub(crate) fn process_memory_mb() -> (f64, f64) {
    (0.0, 0.0)
}

/// The p-th percentile (0-100) of `v`.
fn percentile(v: &[f64], p: f64) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    if s.is_empty() {
        return f64::NAN;
    }
    s[((p / 100.0 * (s.len() - 1) as f64).round() as usize).min(s.len() - 1)]
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

    /// The map's frame of the view centred at the series' centre at `scale` saved as `path` (the vector layers
    /// alone, on black); the series' extent in the frame's pixels.
    fn measure_png(&mut self, path: &str, scale: f64, size: [u32; 2]) -> Option<[f32; 4]> {
        let g = self.grid()?;
        self.map.set_view([g.ow / 2.0, g.oh / 2.0], scale);
        let shown: Vec<bool> = self.layers.iter().map(|l| l.visible).collect();
        for l in &mut self.layers {
            l.visible = false;
        }
        self.map.export_bg = Some([0.0, 0.0, 0.0, 1.0]);
        self.map.dirty = true;
        self.render_map(0, size, [size[0] as f32, size[1] as f32], 1.0);
        let saved = self.gpu.as_ref().and_then(|gpu| gpu.read_target(0));
        self.map.export_bg = None;
        for (l, v) in self.layers.iter_mut().zip(shown) {
            l.visible = v;
        }
        self.map.dirty = true;
        let (w, h, px) = saved?;
        let _ = image::save_buffer(path, &px, w, h, image::ColorType::Rgba8);
        let rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(size[0] as f32, size[1] as f32));
        let (a, b) = (self.map.screen_of(rect, 0.0, 0.0), self.map.screen_of(rect, g.ow, g.oh));
        println!("saved {path}");
        Some([a.x, a.y, b.x, b.y])
    }

    fn measure_vectors_stage(&mut self, st: &mut MeasureVectors, ctx: &egui::Context) -> Option<i32> {
        use serde_json::json;
        match st.stage {
            0 => {
                self.basemap.ui.on = false;
                let s = self.session.as_ref()?;
                if self.opening.is_some() || !s.loaded() {
                    return None;
                }
                let files = self.opts.measure_vector_files.clone();
                st.mem_before = process_memory_mb().0;
                st.t_open = Some(Instant::now());
                self.add_vector_files(files, ctx);
                st.stage = 1;
            }
            1 => {
                // read and placed, then drawn whole: every layer on the GPU (the frames meanwhile draw on the CPU,
                // within their budget)
                st.frames += 1;
                let t = st.t_open?;
                // the memory through the opening: at each change of what is being done
                let doing = if !self.vector_loads.is_empty() || self.vectors.is_empty() {
                    "reading"
                } else if self.vectors.iter().any(|v| v.placed_for == 0) {
                    "placing"
                } else if self.vectors.iter().any(|v| v.gpu.job.is_some()) {
                    "building for the GPU"
                } else {
                    "uploaded"
                };
                if st.timeline.last().is_none_or(|l| l.2 != doing) {
                    st.timeline.push((ms(t), process_memory_mb().0 - st.mem_before, doing));
                }
                if !self.vector_loads.is_empty() || self.vectors.is_empty() {
                    return None;
                }
                if st.read_ms == 0.0 {
                    st.read_ms = ms(t);
                    let mem = process_memory_mb().0 - st.mem_before;
                    println!("read in {:.0} ms: +{mem:.0} MB", st.read_ms);
                    st.json.insert("memory_read_mb".into(), json!(mem));
                }
                if self.vectors.iter().any(|v| v.placed_for == 0) {
                    return None;
                }
                if st.placed_ms == 0.0 {
                    st.placed_ms = ms(t);
                    let mem = process_memory_mb().0 - st.mem_before;
                    println!("placed after {:.0} ms: +{mem:.0} MB", st.placed_ms);
                    st.json.insert("memory_placed_mb".into(), json!(mem));
                }
                if self.gpu.is_some() && !self.vectors.iter().all(|v| v.gpu.draws(v.placed_for)) {
                    if ms(t) < 600_000.0 {
                        return None;
                    }
                    println!("GPU: not built in 10 minutes");
                }
                let whole = ms(t);
                let (mem, peak) = process_memory_mb();
                println!(
                    "read and placed in {:.0} ms; drawn whole after {whole:.0} ms ({} frames meanwhile); memory {:.0} MB \
                     (+{:.0} MB for the layers, peak {peak:.0} MB)",
                    st.placed_ms,
                    st.frames,
                    mem,
                    mem - st.mem_before
                );
                st.json.insert("read_and_placed_ms".into(), json!(st.placed_ms));
                st.json.insert("first_whole_draw_ms".into(), json!(whole));
                st.json.insert("frames_until_whole".into(), json!(st.frames));
                st.json.insert("memory_mb".into(), json!(mem));
                st.json.insert("memory_layers_mb".into(), json!(mem - st.mem_before));
                st.json.insert("memory_peak_mb".into(), json!(peak));
                for (at, mem, doing) in &st.timeline {
                    println!("  {at:6.0} ms: {doing} (+{mem:.0} MB)");
                }
                st.stage = 2;
            }
            2 => {
                // once drawn: the GPU's uploads done (their staging buffers let go)
                if let Some(gpu) = self.gpu.as_ref() {
                    gpu.finish();
                }
                let mem = process_memory_mb().0 - st.mem_before;
                println!("memory once drawn: +{mem:.0} MB for the layers");
                if let Some((a, r)) = self.gpu.as_ref().and_then(|g| g.allocator_mb()) {
                    println!("  the GPU's allocator: {a:.0} MB allocated, {r:.0} MB reserved (in the process' memory)");
                    st.json.insert("gpu_allocated_mb".into(), json!(a));
                    st.json.insert("gpu_reserved_mb".into(), json!(r));
                }
                st.json.insert("memory_drawn_mb".into(), json!(mem));
                let info = self.session.as_ref()?.info.clone();
                println!("series: {} x {} px, {} dates ({})", info.width, info.height, info.t(), info.description);
                let mut layers = Vec::new();
                for i in 0..self.vectors.len() {
                    let mut place = f64::INFINITY;
                    for _ in 0..3 {
                        let t = Instant::now();
                        self.place_vector(i);
                        place = place.min(ms(t));
                    }
                    let v = &self.vectors[i];
                    println!(
                        "{}: {}, {} vertices{}, {}; read in {:.0} ms, placed on the series' grid in {place:.1} ms",
                        v.name,
                        v.data.kind_text(),
                        v.data.vertices,
                        if v.data.truncated { " (read up to the vertex limit)" } else { "" },
                        v.data.crs_name,
                        v.data.load_ms
                    );
                    // the point on each polygon (its label's, the interpretation's): inside it
                    let areas = v.data.features.iter().filter(|f| f.kind == janus_geo::vectors::Kind::Area);
                    let inside = areas.clone().filter(|f| janus_geo::plane::contains(&f.parts, f.cx, f.cy)).count();
                    if v.data.areas > 0 {
                        println!("  the point on each polygon: {inside} of {} inside it", areas.count());
                    }
                    layers.push(json!({
                        "name": v.name, "features": v.data.features.len(), "vertices": v.data.vertices,
                        "truncated": v.data.truncated, "read_ms": v.data.load_ms, "place_ms": place,
                    }));
                }
                st.json.insert("layers".into(), json!(layers));
                // the GPU's build (the one the map made)
                let mut gpu = Vec::new();
                for v in &self.vectors {
                    if let Some(g) = &v.gpu.ready {
                        println!(
                            "{}: built for the GPU in {:.0} ms, {:.1} MB on the GPU; points of each level: {:?}",
                            v.name,
                            g.build_ms,
                            g.bytes as f64 / 1048576.0,
                            g.level_points
                        );
                        gpu.push(json!({"build_ms": g.build_ms, "gpu_mb": g.bytes as f64 / 1048576.0,
                            "level_points": g.level_points}));
                    }
                }
                st.json.insert("gpu".into(), json!(gpu));
                // the CPU's drawing (what the GPU does not draw: a highlighted feature, labels; all of it before the
                // GPU's build): the whole series in view, then 8 points per pixel at its centre
                let canvas = Rect::from_min_max(Pos2::ZERO, Pos2::new(1600.0, 900.0));
                self.map.canvas = [canvas.width(), canvas.height()];
                let g = self.grid()?;
                let whole_scale = 0.98 * (1600.0 / g.ow).min(900.0 / g.oh);
                let layer = LayerId::new(Order::Foreground, Id::new("measure vectors"));
                let mut cpu = serde_json::Map::new();
                for (name, hover) in
                    [("whole series", false), ("zoomed in", false), ("whole series, a feature hovered", true)]
                {
                    if name.starts_with("zoomed") {
                        self.map.set_view([g.ow / 2.0, g.oh / 2.0], 8.0 * g.fx);
                    } else {
                        self.map.set_view([g.ow / 2.0, g.oh / 2.0], whole_scale);
                    }
                    let hovered = hover
                        .then(|| {
                            (0..self.vectors.len()).find_map(|li| {
                                let n = self.vectors[li].data.features.len();
                                (n > 0).then_some((li, n / 2))
                            })
                        })
                        .flatten();
                    let before = std::mem::replace(&mut self.hover_vector, hovered);
                    let (mut times, mut vertices) = (Vec::new(), 0);
                    for _ in 0..9 {
                        let painter = ctx.layer_painter(layer).with_clip_rect(canvas);
                        let m = &self.map;
                        let at = |x: f64, y: f64| m.screen_of(canvas, x / g.fx, y / g.fy);
                        let t = Instant::now();
                        self.draw_vector_marks(&painter, canvas, &at);
                        let shapes =
                            ctx.graphics_mut(|gl| std::mem::take(gl.entry(layer)).all_entries().cloned().collect());
                        let prims = ctx.tessellate(shapes, 1.0);
                        times.push(ms(t));
                        vertices = prims
                            .iter()
                            .map(|p| match &p.primitive {
                                egui::epaint::Primitive::Mesh(m) => m.vertices.len(),
                                _ => 0,
                            })
                            .sum::<usize>();
                    }
                    self.hover_vector = before;
                    let (p50, p95) = (percentile(&times, 50.0), percentile(&times, 95.0));
                    println!(
                        "CPU draw, {name} ({:.2} points per pixel): p50 {p50:.2} ms, p95 {p95:.2} ms per frame (shapes and \
                         their tessellation), {vertices} vertices",
                        self.screen_per_px()
                    );
                    cpu.insert(name.into(), json!({"p50_ms": p50, "p95_ms": p95, "vertices": vertices}));
                }
                st.json.insert("cpu_draw".into(), serde_json::Value::Object(cpu));
                // the GPU's frames: 40 a view, the view panned a little each frame (each frame drawn again)
                let size = [1600, 900];
                let mut frames = serde_json::Map::new();
                for zoom in 0..3 {
                    let k = match zoom {
                        0 => whole_scale,
                        1 => 8.0 * whole_scale,
                        _ => 8.0 * g.fx,
                    };
                    let mut times = Vec::new();
                    for f in 0..40 {
                        let d = (f as f64 - 20.0) * 3.0 / k; // 3 screen points a frame
                        self.map.set_view([g.ow / 2.0 + d, g.oh / 2.0 + d * 0.5], k);
                        self.map.dirty = true;
                        let t = Instant::now();
                        self.render_map(0, size, [1600.0, 900.0], 1.0);
                        if let Some(gpu) = self.gpu.as_ref() {
                            gpu.finish();
                        }
                        times.push(ms(t));
                    }
                    let name = ["whole series", "8x closer", "zoomed in"][zoom];
                    let (p50, p95) = (percentile(&times, 50.0), percentile(&times, 95.0));
                    println!(
                        "GPU draw, {name} ({:.3} points per pixel): p50 {p50:.2} ms, p95 {p95:.2} ms per frame (series and \
                         vectors, finished; panning)",
                        self.screen_per_px()
                    );
                    frames.insert(name.into(), json!({"p50_ms": p50, "p95_ms": p95}));
                }
                st.json.insert("gpu_frames".into(), serde_json::Value::Object(frames));
                // JANUS_MEASURE_PNG: the whole series' frame and one 8x closer saved there
                if let Ok(path) = std::env::var("JANUS_MEASURE_PNG") {
                    let whole = self.measure_png(&path, whole_scale, size);
                    let closer = self.measure_png(&path.replace(".png", "_8x.png"), 8.0 * whole_scale, size);
                    st.json.insert("series_in_frame".into(), json!(whole));
                    st.json.insert("series_in_frame_8x".into(), json!(closer));
                }
                self.map.set_view([g.ow / 2.0, g.oh / 2.0], whole_scale);
                // picking: the feature under 2000 places, the whole series in view and zoomed in
                for (name, k) in [("whole series", whole_scale), ("zoomed in", 8.0 * g.fx)] {
                    self.map.set_view([g.ow / 2.0, g.oh / 2.0], k);
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
                    let us = ms(t) * 1000.0 / 2000.0;
                    println!("picking, {name}: {us:.1} us per place ({hits} of 2000 on a feature)");
                    st.json.insert(format!("picking_us, {name}"), json!(us));
                }
                self.map.set_view([g.ow / 2.0, g.oh / 2.0], whole_scale);
                // the series of every feature, each layer to its own table (layers of up to 100 000 features)
                st.dir = std::env::temp_dir().join("janus-measure-vectors");
                let _ = std::fs::create_dir_all(&st.dir);
                for i in 0..self.vectors.len() {
                    if self.vectors[i].data.features.len() > 100_000 {
                        println!("{}: its series not extracted (over 100 000 features)", self.vectors[i].name);
                        continue;
                    }
                    let path = st.dir.join(format!("{}.csv", self.vectors[i].id)).to_string_lossy().into_owned();
                    st.jobs.extend(self.extract_series(i, false, true, path, ctx));
                }
                st.stage = 3;
            }
            _ => {
                for j in &st.jobs {
                    j.wait();
                    let kb = std::fs::metadata(&j.path).map_or(0.0, |m| m.len() as f64 / 1024.0);
                    println!("{}: {:.2} s, {kb:.0} KB {}", j.title, j.seconds(), j.error());
                }
                let _ = std::fs::remove_dir_all(&st.dir);
                if let Ok(path) = std::env::var("JANUS_MEASURE_JSON") {
                    let text = serde_json::to_string_pretty(&serde_json::Value::Object(std::mem::take(&mut st.json)));
                    let _ = std::fs::write(&path, text.unwrap_or_default());
                    println!("saved {path}");
                }
                return Some(0);
            }
        }
        None
    }
}
