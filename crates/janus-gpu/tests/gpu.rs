//! The renderer without a window: the statistics against the same sums on the CPU, and the map's colours against
//! the colormap (the C++'s self-test checks the map the same way, app_selftest.cpp).

use janus_gpu::{DrawParams, Gpu, Mode, colormaps};

fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
    let limits = janus_gpu::required_limits(&adapter);
    pollster::block_on(
        adapter.request_device(&wgpu::DeviceDescriptor { required_limits: limits, ..Default::default() }),
    )
    .ok()
}

/// The C++ shader's statistics of one series, in f32 as the GPU computes them.
fn cpu_stats(times: &[f32], v: &[f32]) -> [f32; 8] {
    let (mut n, mut sv, mut sx, mut mn, mut mx) = (0f32, 0f32, 0f32, 3.0e38f32, -3.0e38f32);
    let (mut last, mut drop) = (0f32, 0f32);
    for (&x, &y) in times.iter().zip(v) {
        if x.is_nan() || y.is_nan() {
            continue;
        }
        if n > 0.0 && last - y > drop {
            drop = last - y;
        }
        last = y;
        n += 1.0;
        sv += y;
        sx += x;
        mn = mn.min(y);
        mx = mx.max(y);
    }
    let (mv, mxx) = (sv / n, sx / n);
    let (mut sxx, mut sxy, mut syy) = (0f32, 0f32, 0f32);
    for (&x, &y) in times.iter().zip(v) {
        if x.is_nan() || y.is_nan() {
            continue;
        }
        let (dx, dy) = (x - mxx, y - mv);
        sxx += dx * dx;
        sxy += dx * dy;
        syy += dy * dy;
    }
    [mv, (syy / (n - 1.0)).sqrt(), sxy / sxx, n, mn, mx, (sxy * sxy) / (sxx * syy), drop]
}

#[test]
fn statistics_and_map() {
    let Some((device, queue)) = device() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    let (w, h, t) = (37usize, 23usize, 9usize);
    let times: Vec<f32> = (0..t).map(|i| 2000.0 + i as f32).collect();
    let value = |i: usize, p: usize| -> f32 {
        if (p + i).is_multiple_of(11) { f32::NAN } else { ((p * 31 + i * 17) % 97) as f32 / 97.0 + i as f32 * 0.01 }
    };
    let mut gpu = Gpu::new(&device, &queue);
    gpu.create_cube(w as u32, h as u32, t as u32, &times);
    for i in 0..t {
        let layer: Vec<f32> = (0..w * h).map(|p| value(i, p)).collect();
        gpu.upload_layer(i as u32, &layer);
    }
    let stats = gpu.compute_stats().unwrap();
    for p in 0..w * h {
        let series: Vec<f32> = (0..t).map(|i| value(i, p)).collect();
        let want = cpu_stats(&times, &series);
        let got = [
            stats.s0[p][0],
            stats.s0[p][1],
            stats.s0[p][2],
            stats.s0[p][3],
            stats.s1[p][0],
            stats.s1[p][1],
            stats.s1[p][2],
            stats.s1[p][3],
        ];
        for (k, (g, e)) in got.iter().zip(want).enumerate() {
            assert!((g - e).abs() <= 1e-5 * e.abs().max(1.0), "pixel {p}, statistic {k}: {g} vs {e}");
        }
    }

    // the map of date 3 drawn 1:1, against the colormap's colours
    gpu.ensure_target(0, [w as u32, h as u32]);
    gpu.set_colormap(colormaps::VIRIDIS);
    let p = DrawParams {
        rect: [0.0, 0.0, w as f32, h as f32],
        mode: Mode::Value,
        layers: [3, 0, 0],
        range: [0.0, 1.0],
        alpha: 1.0,
    };
    gpu.render(0, Some(&p));
    let (_, _, px) = gpu.read_target(0).unwrap();
    let lut = colormaps::all()[colormaps::VIRIDIS].lut();
    for y in 0..h {
        for x in 0..w {
            let v = value(3, y * w + x);
            let got = &px[(y * w + x) * 4..(y * w + x) * 4 + 4];
            if v.is_nan() {
                assert_eq!(got[3], 0, "nodata is left transparent");
                continue;
            }
            // linear filtering between the two nearest of the 256 colours
            let f = v.clamp(0.0, 1.0) * 255.0;
            let (a, b) = (lut[f.floor() as usize], lut[f.ceil() as usize]);
            for c in 0..3 {
                let (lo, hi) = (a[c].min(b[c]) as i32 - 2, a[c].max(b[c]) as i32 + 2);
                assert!((lo..=hi).contains(&(got[c] as i32)), "({x}, {y}) channel {c}: {} not in {lo}..{hi}", got[c]);
            }
            assert_eq!(got[3], 255);
        }
    }
}
