//! The renderer without a window: the statistics against the same sums on the CPU, the map's colours against the
//! colormap (the C++'s self-test checks the map the same way, app_selftest.cpp), detail tiles, class colours,
//! images and the blending of layers.

use janus_gpu::{Blend, CLASS_LUT_SIZE, Gpu, LayerParams, Mode, colormaps};

/// One test on the GPU at a time: devices made side by side crash Windows' software adapter (WARP) on the CI's
/// runners (STATUS_ACCESS_VIOLATION once there were four tests). The guard comes first, so it is dropped last.
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

/// A colour between the two nearest of the colormap's 256 (bilinear), give or take 2.
fn near_colormap(got: &[u8], lut: &[[u8; 4]; 256], v: f32) -> bool {
    let f = v.clamp(0.0, 1.0) * 255.0;
    let (a, b) = (lut[f.floor() as usize], lut[f.ceil() as usize]);
    (0..3).all(|c| {
        let (lo, hi) = (a[c].min(b[c]) as i32 - 2, a[c].max(b[c]) as i32 + 2);
        (lo..=hi).contains(&(got[c] as i32))
    })
}

#[test]
fn statistics_and_map() {
    let Some((_one, device, queue, backend)) = device() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    let (w, h, t) = (37usize, 23usize, 9usize);
    let times: Vec<f32> = (0..t).map(|i| 2000.0 + i as f32).collect();
    let value = |i: usize, p: usize| -> f32 {
        if (p + i).is_multiple_of(11) { f32::NAN } else { ((p * 31 + i * 17) % 97) as f32 / 97.0 + i as f32 * 0.01 }
    };
    let mut gpu = Gpu::new(&device, &queue);
    gpu.set_backend(backend);
    let mut cube = gpu.create_cube(w as u32, h as u32, t as u32, &times);
    for i in 0..t {
        let layer: Vec<f32> = (0..w * h).map(|p| value(i, p)).collect();
        cube.upload_layer(i as u32, &layer);
    }
    let stats = gpu.compute_stats_async(&cube, || {}).wait().unwrap();
    for p in 0..w * h {
        let series: Vec<f32> = (0..t).map(|i| value(i, p)).collect();
        let want = cpu_stats(&times, &series);
        let (s0, s1) = (stats.s0[p], stats.s1[p]);
        let got = [s0[0], s0[1], s0[2], s0[3], s1[0], s1[1], s1[2], s1[3]];
        for (k, (g, e)) in got.iter().zip(want).enumerate() {
            assert!((g - e).abs() <= 1e-5 * e.abs().max(1.0), "pixel {p}, statistic {k}: {g} vs {e}");
        }
    }

    // a season: the dates outside it have a NaN time and are left out of the statistics
    let season: Vec<f32> = times.iter().enumerate().map(|(i, &x)| if i % 3 == 1 { f32::NAN } else { x }).collect();
    cube.set_times(&season);
    let stats = gpu.compute_stats_async(&cube, || {}).wait().unwrap();
    for p in 0..w * h {
        let series: Vec<f32> = (0..t).map(|i| value(i, p)).collect();
        let want = cpu_stats(&season, &series);
        let (s0, s1) = (stats.s0[p], stats.s1[p]);
        let got = [s0[0], s0[1], s0[2], s0[3], s1[0], s1[1], s1[2], s1[3]];
        for (k, (g, e)) in got.iter().zip(want).enumerate() {
            let same = (g.is_nan() && e.is_nan()) || (g - e).abs() <= 1e-5 * e.abs().max(1.0);
            assert!(same, "season: pixel {p}, statistic {k}: {g} vs {e}");
        }
    }
    cube.set_times(&times);

    // the map of date 3 drawn 1:1, against the colormap's colours
    let lut = colormaps::all()[colormaps::VIRIDIS].lut();
    let rect = [0.0, 0.0, w as f32, h as f32];
    let mut f = gpu.begin_map(0, [w as u32, h as u32], [0.0; 4]);
    gpu.draw_cube(&mut f, &cube, rect, &LayerParams::new(Mode::Value, [3, 0, 0], [0.0, 1.0]), colormaps::VIRIDIS, 1.0);
    gpu.end_map(f);
    let (_, _, px) = gpu.read_target(0).unwrap();
    for p in 0..w * h {
        let (v, got) = (value(3, p), &px[p * 4..p * 4 + 4]);
        if v.is_nan() {
            assert_eq!(got[3], 0, "nodata is left transparent");
        } else {
            assert!(near_colormap(got, &lut, v), "pixel {p}: {got:?} for {v}");
            assert_eq!(got[3], 255);
        }
    }

    // a detail tile in the Value mode, over the cube: its own values
    let tile: Vec<f32> = (0..4 * 4).map(|i| i as f32 / 15.0).collect();
    let tex = gpu.create_tile(4, 4, &tile);
    let mut f = gpu.begin_map(1, [8, 8], [0.0; 4]);
    gpu.draw_tile(
        &mut f,
        &cube,
        &tex,
        [0.0, 0.0, 8.0, 8.0],
        &LayerParams::new(Mode::Value, [0, 0, 0], [0.0, 1.0]),
        colormaps::VIRIDIS,
        1.0,
    );
    gpu.end_map(f);
    let (_, _, px) = gpu.read_target(1).unwrap();
    for y in 0..8 {
        for x in 0..8 {
            let v = tile[(y / 2) * 4 + x / 2];
            assert!(near_colormap(&px[(y * 8 + x) * 4..], &lut, v), "tile ({x}, {y})");
        }
    }

    // classes: value -> its colour, a value without one transparent
    let mut table = vec![0u8; CLASS_LUT_SIZE as usize * 4];
    table[4..8].copy_from_slice(&[10, 200, 30, 255]); // class 1
    let classes: Vec<f32> = vec![1.0, 2.0, 1.0, f32::NAN];
    let mut cc = gpu.create_cube(2, 2, 1, &[0.0]);
    cc.upload_layer(0, &classes);
    let mut p = LayerParams::new(Mode::Value, [0, 0, 0], [0.0, 1.0]);
    p.class_lut = Some(gpu.create_class_lut(&table));
    let mut f = gpu.begin_map(2, [2, 2], [0.0; 4]);
    gpu.draw_cube(&mut f, &cc, [0.0, 0.0, 2.0, 2.0], &p, colormaps::VIRIDIS, 1.0);
    gpu.end_map(f);
    let (_, _, px) = gpu.read_target(2).unwrap();
    assert_eq!(&px[0..4], &[10, 200, 30, 255]);
    assert_eq!(px[7], 0, "class 2 has no colour");
    assert_eq!(&px[8..12], &[10, 200, 30, 255]);
    assert_eq!(px[15], 0, "nodata");

    // an image, then a half-transparent rectangle over it: blended in sRGB as GL blends
    let img = gpu.create_image(1, 1, &[255, 0, 0, 255]);
    let mut f = gpu.begin_map(3, [2, 2], [0.0; 4]);
    gpu.draw_image(&mut f, &img, [0.0, 0.0, 2.0, 2.0], 1.0, None, true);
    gpu.fill_rect(&mut f, [0.0, 0.0, 2.0, 2.0], [0.0, 0.0, 1.0, 0.5]);
    gpu.end_map(f);
    let (_, _, px) = gpu.read_target(3).unwrap();
    assert!((px[0] as i32 - 128).abs() <= 1 && px[1] == 0 && (px[2] as i32 - 128).abs() <= 1, "{:?}", &px[0..4]);
}

#[test]
fn blend_modes() {
    let Some((_one, device, queue, backend)) = device() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    let mut gpu = Gpu::new(&device, &queue);
    gpu.set_backend(backend);
    // a colour at half opacity over another, in each mode, against the formula on the bytes (the target keeps
    // sRGB bytes as they are)
    let (d, c, a) = ([0.4f32, 0.6, 0.2], [0.5f32, 0.5, 1.0], 0.5f32);
    let want = |b: Blend, d: f32, c: f32| match b {
        Blend::Normal => d * (1.0 - a) + c * a,
        Blend::Multiply => d * (1.0 - a + c * a),
        Blend::Screen => d + c * a * (1.0 - d),
        Blend::Add => (d + c * a).min(1.0),
        Blend::Darken => d.min(1.0 - a + c * a),
        Blend::Lighten => d.max(c * a),
    };
    for b in Blend::ALL {
        let mut f = gpu.begin_map(0, [4, 4], [d[0], d[1], d[2], 1.0]);
        f.blend = b;
        gpu.fill_rect(&mut f, [0.0, 0.0, 4.0, 4.0], [c[0], c[1], c[2], a]);
        gpu.end_map(f);
        let (_, _, px) = gpu.read_target(0).unwrap();
        for k in 0..3 {
            let e = want(b, d[k], c[k]) * 255.0;
            assert!((px[k] as f32 - e).abs() <= 2.0, "{}: channel {k} {} vs {e}", b.name(), px[k]);
        }
        assert_eq!(px[3], 255, "{}: the target's alpha kept", b.name());
    }
    assert_eq!(Blend::parse(" multiply "), Some(Blend::Multiply));
}

#[test]
fn similarity_map() {
    let Some((_one, device, queue, backend)) = device() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    let (w, h, t) = (19usize, 13usize, 12usize);
    let times: Vec<f32> = (0..t).map(|i| 2000.0 + i as f32).collect();
    let value = |i: usize, p: usize| -> f32 {
        if (p + i).is_multiple_of(7) {
            f32::NAN
        } else {
            0.4 + 0.3 * ((i as f32) * 0.9 + p as f32 * 0.37).sin() + 0.002 * p as f32
        }
    };
    let mut gpu = Gpu::new(&device, &queue);
    gpu.set_backend(backend);
    let mut cube = gpu.create_cube(w as u32, h as u32, t as u32, &times);
    for i in 0..t {
        let layer: Vec<f32> = (0..w * h).map(|p| value(i, p)).collect();
        cube.upload_layer(i as u32, &layer);
    }
    let reference: Vec<f32> = (0..t).map(|i| value(i, 40)).collect();
    let scale = 0.6f32;
    cube.set_reference(&reference, scale);
    // the CPU's: the values 1 - RMS difference / scale, the shape the correlation
    let cpu = |p: usize, shape: bool| -> f32 {
        let pairs: Vec<(f64, f64)> = (0..t)
            .map(|i| (f64::from(value(i, p)), f64::from(reference[i])))
            .filter(|(a, b)| !a.is_nan() && !b.is_nan())
            .collect();
        let n = pairs.len() as f64;
        if n < 3.0 {
            return f32::NAN;
        }
        let (ma, mb) = (pairs.iter().map(|x| x.0).sum::<f64>() / n, pairs.iter().map(|x| x.1).sum::<f64>() / n);
        let (mut aa, mut bb, mut ab, mut dd) = (0.0, 0.0, 0.0, 0.0);
        for (a, b) in &pairs {
            aa += (a - ma) * (a - ma);
            bb += (b - mb) * (b - mb);
            ab += (a - ma) * (b - mb);
            dd += (a - b) * (a - b);
        }
        if shape { (ab / (aa * bb).sqrt()) as f32 } else { (1.0 - (dd / n).sqrt() / f64::from(scale)) as f32 }
    };
    let lut = colormaps::all()[colormaps::VIRIDIS].lut();
    for shape in [false, true] {
        let rect = [0.0, 0.0, w as f32, h as f32];
        let mut f = gpu.begin_map(0, [w as u32, h as u32], [0.0; 4]);
        let params = LayerParams::new(Mode::Similarity, [i32::from(shape), 0, 0], [-1.0, 1.0]);
        gpu.draw_cube(&mut f, &cube, rect, &params, colormaps::VIRIDIS, 1.0);
        gpu.end_map(f);
        let (_, _, px) = gpu.read_target(0).unwrap();
        for p in 0..w * h {
            let want = cpu(p, shape);
            let got = &px[p * 4..p * 4 + 4];
            assert!(near_colormap(got, &lut, (want + 1.0) / 2.0), "shape {shape}, pixel {p}: {got:?} for {want}");
        }
        // the reference's own pixel: the same series
        let at = &px[40 * 4..40 * 4 + 4];
        assert!(near_colormap(at, &lut, 1.0), "shape {shape}: the reference's pixel {at:?}");
    }
}

#[test]
fn phase_map() {
    let Some((_one, device, queue, backend)) = device() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    // 3 years of 20 irregular dates each; every pixel peaks at its own time of year, with its own amplitude and mean
    let (w, h, t) = (17usize, 11usize, 60usize);
    // years from the first date, on day 140
    let times: Vec<f32> = (0..t).map(|i| i as f32 / 20.0 + 0.013 * ((i * 7) % 5) as f32).collect();
    let offset = 140.0f32 / 365.25;
    let value = |i: usize, p: usize| -> f32 {
        if (p + 3 * i).is_multiple_of(11) {
            return f32::NAN;
        }
        let peak = p as f32 / (w * h) as f32;
        let amp = 0.05 + 0.25 * ((p * 13) % 7) as f32 / 6.0;
        let mean = 0.2 + 0.5 * ((p * 5) % 9) as f32 / 8.0;
        let year = (times[i] + offset).rem_euclid(1.0);
        mean + amp * (std::f32::consts::TAU * (year - peak)).cos() + 0.01 * (i as f32 * 1.7).sin()
    };
    let mut gpu = Gpu::new(&device, &queue);
    gpu.set_backend(backend);
    let mut cube = gpu.create_cube(w as u32, h as u32, t as u32, &times);
    for i in 0..t {
        let layer: Vec<f32> = (0..w * h).map(|p| value(i, p)).collect();
        cube.upload_layer(i as u32, &layer);
    }
    let (lo, hi, amp_hi) = (0.1f32, 0.8f32, 0.5f32);
    for by_mean in [true, false] {
        let rect = [0.0, 0.0, w as f32, h as f32];
        let mut f = gpu.begin_map(0, [w as u32, h as u32], [0.0; 4]);
        let (mlo, mhi) = if by_mean { (lo, hi) } else { (f32::NAN, f32::NAN) };
        let layers = [offset.to_bits() as i32, mlo.to_bits() as i32, mhi.to_bits() as i32];
        let params = LayerParams::new(Mode::Phase, layers, [0.0, amp_hi]);
        gpu.draw_cube(&mut f, &cube, rect, &params, colormaps::VIRIDIS, 1.0);
        gpu.end_map(f);
        let (_, _, px) = gpu.read_target(0).unwrap();
        for p in 0..w * h {
            let [peak, amp, mean] = janus_gpu::phase::harmonic(&times, offset, |i| value(i, p)).expect("a fit");
            // the peak found where it was put
            let put = p as f32 / (w * h) as f32;
            let off = (peak - put).abs();
            assert!(off.min(1.0 - off) < 0.01, "pixel {p}: peak {peak} for {put}");
            let bright = if by_mean { 0.25 + 0.75 * ((mean - lo) / (hi - lo)).clamp(0.0, 1.0) } else { 1.0 };
            let want = janus_gpu::phase::hsv(peak, (amp / amp_hi).clamp(0.0, 1.0), bright);
            let got = &px[p * 4..p * 4 + 4];
            for c in 0..3 {
                let e = want[c] * 255.0;
                assert!((f32::from(got[c]) - e).abs() <= 2.5, "pixel {p} (mean {by_mean}): {got:?} for {want:?}");
            }
            assert_eq!(got[3], 255);
        }
    }
}
