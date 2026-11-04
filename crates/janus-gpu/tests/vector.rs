//! Vector lines drawn without a window, against the same lines drawn on the CPU: a layer's lines are the union of
//! their segments (each a capsule: the points within a distance of it), each pixel shaded once by the segment
//! nearest to it. Overlapping quads blended one over the other made bright and dark specks where a line turns back
//! on itself, where vertices almost touch and along runs of segments shorter than a pixel, changing as the map was
//! panned (EU_Vector's GADM and WDPA outlines).
//!
//! JANUS_VECTOR_DUMP=FOLDER writes the GPU's and the CPU's images (PPM) for a look.

// draw_vector takes the ranges of points drawn: one range here
#![allow(clippy::single_range_in_vec_init)]

use janus_gpu::{Gpu, VectorKind, VectorPoint, VectorStyle, VectorView};

static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn device() -> Option<(std::sync::MutexGuard<'static, ()>, wgpu::Device, wgpu::Queue, wgpu::Backend)> {
    let one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
    let limits = janus_gpu::required_limits(&adapter);
    let (d, q) = pollster::block_on(
        adapter.request_device(&wgpu::DeviceDescriptor { required_limits: limits, ..Default::default() }),
    )
    .ok()?;
    Some((one, d, q, adapter.get_info().backend))
}

const W: u32 = 200;
const H: u32 = 120;
const FEATHER: f32 = 0.6;
const BG: [f32; 3] = [0.1, 0.12, 0.15];
const COLORS: [[u8; 4]; 3] = [[255, 220, 40, 255], [80, 200, 255, 255], [255, 90, 160, 255]];

/// The lines (in target pixels; NaN breaks them) and each point's feature.
fn shapes() -> Vec<VectorPoint> {
    let mut pts = Vec::new();
    let mut line = |p: &[(f32, f32)], f: u32| {
        pts.extend(p.iter().map(|&(x, y)| VectorPoint { x, y, f }));
        pts.push(VectorPoint::BREAK);
    };
    // a spike: out and back almost on itself
    line(&[(10.0, 100.0), (60.0, 15.0), (14.0, 102.0), (40.0, 110.0)], 0);
    // a ring pinched: two of its vertices 0.3 px apart, one vertex twice
    line(
        &[
            (80.0, 20.0),
            (120.0, 20.0),
            (100.5, 60.0),
            (100.5, 60.0),
            (130.0, 100.0),
            (70.0, 100.0),
            (100.2, 60.1),
            (80.0, 20.0),
        ],
        1,
    );
    // a coast: runs of segments shorter than a pixel, jittered
    let mut coast = Vec::new();
    for i in 0..400 {
        let t = i as f32 * 0.12;
        let j = ((i * 7919) % 13) as f32 / 13.0 - 0.5;
        coast.push((145.0 + t, 60.0 + 30.0 * (t * 0.15).sin() + 0.6 * j));
    }
    line(&coast, 2);
    pts
}

fn style(ppp: f32) -> VectorStyle {
    VectorStyle { width: ppp, halo: ppp, halo_color: [0.0, 0.0, 0.0, 170.0 / 255.0], alpha: 1.0 }
}

/// The lines as they should look: each pixel's centre shaded by its nearest segment (the shader's shade()).
fn reference(pts: &[VectorPoint], s: &VectorStyle, dx: f32) -> Vec<[f32; 3]> {
    let mut out = vec![BG; (W * H) as usize];
    let core = s.width * 0.5;
    let outer = core + s.halo;
    for y in 0..H {
        for x in 0..W {
            let p = (x as f32 + 0.5 - dx, y as f32 + 0.5);
            let mut best = (f32::INFINITY, 0u32);
            for w in pts.windows(2) {
                let (a, b) = (w[0], w[1]);
                if a.x.is_nan() || b.x.is_nan() {
                    continue;
                }
                let (vx, vy) = (b.x - a.x, b.y - a.y);
                let l2 = vx * vx + vy * vy;
                let t = if l2 > 0.0 { (((p.0 - a.x) * vx + (p.1 - a.y) * vy) / l2).clamp(0.0, 1.0) } else { 0.0 };
                let d = ((p.0 - a.x - t * vx).powi(2) + (p.1 - a.y - t * vy).powi(2)).sqrt();
                if d < best.0 {
                    best = (d, a.f);
                }
            }
            let e = best.0;
            let k = ((outer + FEATHER - e) / FEATHER).clamp(0.0, 1.0);
            if k <= 0.0 {
                continue;
            }
            let col = COLORS[best.1 as usize].map(|v| f32::from(v) / 255.0);
            let m = ((e - core) / FEATHER + 0.5).clamp(0.0, 1.0);
            let c: [f32; 4] = std::array::from_fn(|i| col[i] + (s.halo_color[i] - col[i]) * m);
            let a = c[3] * k * s.alpha;
            out[(y * W + x) as usize] = std::array::from_fn(|i| c[i] * a + BG[i] * (1.0 - a));
        }
    }
    out
}

fn dump(dir: &str, name: &str, px: impl Iterator<Item = [u8; 3]>) {
    let mut bytes = format!("P6\n{W} {H}\n255\n").into_bytes();
    px.for_each(|p| bytes.extend_from_slice(&p));
    std::fs::write(std::path::Path::new(dir).join(name), bytes).unwrap();
}

#[test]
fn lines_blended_once() {
    let Some((_one, device, queue, backend)) = device() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    let mut gpu = Gpu::new(&device, &queue);
    gpu.set_backend(backend);
    let pts = shapes();
    let colors: Vec<u32> = COLORS.iter().map(|c| u32::from_le_bytes(*c)).collect();
    let features = gpu.create_vector_features(&colors, &[0, 0, 0], &[[0.0, 0.0]]);
    let g = gpu.upload_vector_geometry(&pts, &[], &features);
    let dir = std::env::var("JANUS_VECTOR_DUMP").ok();
    let mut worst = Vec::new();
    // the screen's scales, the map panned by fractions of a pixel
    for ppp in [1.0f32, 1.5, 2.0] {
        for dx in [0.0f32, 0.25, 0.5, 0.75] {
            let mut f = gpu.begin_map(0, [W, H], [BG[0], BG[1], BG[2], 1.0]);
            let view = VectorView { a: [1.0, 1.0], centre: [0.0, 0.0], t0: [f64::from(dx), 0.0] };
            let s = style(ppp);
            gpu.draw_vector(&mut f, &g, VectorKind::Lines, vec![0..pts.len() as u32], &view, &s);
            gpu.end_map(f);
            let (_, _, px) = gpu.read_target(0).unwrap();
            let want = reference(&pts, &s, dx);
            let mut bad = 0;
            let mut max = 0.0f32;
            for (i, w) in want.iter().enumerate() {
                let d = (0..3).map(|k| (f32::from(px[i * 4 + k]) - w[k] * 255.0).abs()).fold(0.0, f32::max);
                max = max.max(d);
                bad += usize::from(d > 3.0);
            }
            if let Some(dir) = &dir {
                let tag = format!("{ppp}-{dx}");
                dump(dir, &format!("gpu-{tag}.ppm"), px.chunks(4).map(|c| [c[0], c[1], c[2]]));
                dump(dir, &format!("cpu-{tag}.ppm"), want.iter().map(|w| w.map(|v| (v * 255.0).round() as u8)));
            }
            worst.push((ppp, dx, bad, max));
        }
    }
    for &(ppp, dx, bad, max) in &worst {
        eprintln!("width {ppp} px, panned {dx} px: {bad} pixels off by more than 3 (at most {max:.0})");
    }
    for &(ppp, dx, bad, _) in &worst {
        assert!(bad == 0, "width {ppp} px, panned {dx} px: {bad} pixels differ from the lines' union");
    }
}

#[test]
fn layers_drawn_in_order() {
    let Some((_one, device, queue, backend)) = device() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    let mut gpu = Gpu::new(&device, &queue);
    gpu.set_backend(backend);
    let colors: Vec<u32> = COLORS.iter().map(|c| u32::from_le_bytes(*c)).collect();
    let features = gpu.create_vector_features(&colors, &[0, 0, 0], &[[0.0, 0.0]]);
    let p = |x, y, f| VectorPoint { x, y, f };
    // a horizontal line, then a vertical one over it (another layer), then a rectangle over the left half
    let h = gpu.upload_vector_geometry(&[p(10.0, 30.5, 0), p(110.0, 30.5, 0)], &[], &features);
    let v = gpu.upload_vector_geometry(&[p(60.5, 5.0, 1), p(60.5, 55.0, 1)], &[], &features);
    let view = VectorView { a: [1.0, 1.0], centre: [0.0, 0.0], t0: [0.0, 0.0] };
    let s = style(2.0);
    let mut f = gpu.begin_map(0, [W, H], [BG[0], BG[1], BG[2], 1.0]);
    gpu.draw_vector(&mut f, &h, VectorKind::Lines, vec![0..2], &view, &s);
    gpu.draw_vector(&mut f, &v, VectorKind::Lines, vec![0..2], &view, &s);
    gpu.fill_rect(&mut f, [0.0, 0.0, 30.0, f32::from(H as u16)], [1.0, 1.0, 1.0, 1.0]);
    gpu.end_map(f);
    let (_, _, px) = gpu.read_target(0).unwrap();
    let at = |x: u32, y: u32| {
        let i = ((y * W + x) * 4) as usize;
        [px[i], px[i + 1], px[i + 2]]
    };
    // the crossing in the later layer's colour, the earlier layer's line under its rim darkened
    assert_eq!(at(60, 30), [COLORS[1][0], COLORS[1][1], COLORS[1][2]]);
    let rim = at(62, 30);
    assert!(rim.iter().all(|&c| c < 120), "the later layer's rim over the earlier line: {rim:?}");
    assert_eq!(at(90, 30), [COLORS[0][0], COLORS[0][1], COLORS[0][2]], "the earlier line away from the crossing");
    // the raster draw pushed after them covers them
    assert_eq!(at(20, 30), [255, 255, 255]);
}
