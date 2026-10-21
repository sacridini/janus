//! The map on the GPU with wgpu (src/gpu.hpp, gpu_gl.cpp, gpu_metal.mm): the overview as an R32F texture array,
//! the per-pixel temporal statistics in one compute pass, and the display of a date, a statistic or a composite
//! through a colormap into an offscreen target per map panel. One shader source (WGSL) for every system.

pub mod colormaps;

use std::collections::HashMap;
use std::time::Instant;

use wgpu::util::DeviceExt;

/// What the map shows (gpu.hpp, the C++'s display modes 0-13).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum Mode {
    Value = 0,
    Anomaly = 1,
    Mean = 2,
    Std = 3,
    Slope = 4,
    Min = 5,
    Max = 6,
    Amplitude = 7,
    R2 = 8,
    Rgb = 9,
    Diff = 10,
    DropDate = 11,
    DropMag = 12,
    Composite = 13,
}

impl Mode {
    pub const ALL: [Mode; 14] = [
        Mode::Value,
        Mode::Anomaly,
        Mode::Mean,
        Mode::Std,
        Mode::Slope,
        Mode::Min,
        Mode::Max,
        Mode::Amplitude,
        Mode::R2,
        Mode::Rgb,
        Mode::Diff,
        Mode::DropDate,
        Mode::DropMag,
        Mode::Composite,
    ];

    /// Modes read from the statistics (valid once the overview is complete).
    pub fn needs_stats(self) -> bool {
        !matches!(self, Mode::Value | Mode::Rgb | Mode::Diff | Mode::Composite)
    }
}

/// How a layer is drawn into a target.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DrawParams {
    /// where the overview goes in the target, in its pixels: x0, y0, x1, y1 (may extend past the target)
    pub rect: [f32; 4],
    pub mode: Mode,
    /// the date (Value, Anomaly), the two dates (Diff: date, reference), three dates (Rgb), or the composite's
    /// first and last date and reducer
    pub layers: [i32; 3],
    pub range: [f32; 2],
    pub alpha: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ViewUniform {
    rect: [f32; 4],
    layers: [i32; 4],
    range: [f32; 2],
    alpha: f32,
    mode: i32,
    t: u32,
    nan_bits: u32,
    _pad: [u32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct StatsUniform {
    t: u32,
    nan_bits: u32,
    _pad: [u32; 2],
}

const NAN_BITS: u32 = 0x7fc0_0000;
/// The target's format: sRGB, as egui's own textures. The shader works on the colormap's sRGB colours (as the C++)
/// and outputs them linear, which the target stores back as the same sRGB bytes. (A linear target viewed as sRGB
/// would need view formats, which wgpu's OpenGL backend lacks.)
pub const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// The rows of a mapped readback buffer copied once, out of their padding, into a vector (aligned for floats).
fn unpad<T: bytemuck::Pod + Default>((buf, row): &(wgpu::Buffer, u32), w: usize, h: usize) -> Option<Vec<T>> {
    let data = buf.slice(..).get_mapped_range().ok()?;
    let mut out = vec![T::default(); w * h];
    let bytes: &mut [u8] = bytemuck::cast_slice_mut(&mut out);
    let n = w * size_of::<T>();
    for (y, dst) in bytes.chunks_exact_mut(n).enumerate() {
        let s = y * *row as usize;
        dst.copy_from_slice(&data[s..s + n]);
    }
    Some(out)
}

/// Statistics being computed: [`StatsJob::poll`] has them once read back.
pub struct StatsJob {
    rx: std::sync::mpsc::Receiver<Option<Stats>>,
}

impl StatsJob {
    /// The statistics, once ready (`Some(None)`: they could not be read).
    pub fn poll(&self) -> Option<Option<Stats>> {
        self.rx.try_recv().ok()
    }
}

/// The per-pixel statistics read back to the CPU (for the ranges, histograms and the Statistics panel).
#[derive(Default)]
pub struct Stats {
    pub w: usize,
    pub h: usize,
    /// mean, std, slope, n per pixel
    pub s0: Vec<[f32; 4]>,
    /// min, max, R2, largest drop per pixel
    pub s1: Vec<[f32; 4]>,
    /// date index of the largest drop
    pub s2: Vec<f32>,
    /// the whole time (ms): compute, readback and copy
    pub ms: f64,
    /// until the GPU had computed and copied them to readable memory (ms)
    pub gpu_ms: f64,
}

struct GpuCube {
    w: u32,
    h: u32,
    t: u32,
    texture: wgpu::Texture,
    stats: [wgpu::Texture; 3],
    view: wgpu::TextureView,
    stat_views: [wgpu::TextureView; 3],
    times: wgpu::Buffer,
    stats_group: wgpu::BindGroup,
    /// changes with every new cube: the targets' bind groups are made again
    generation: u64,
}

/// An offscreen target (a map panel): an RGBA texture the size of the panel in pixels.
pub struct Target {
    pub texture: wgpu::Texture,
    /// drawn into, and sampled by egui
    pub view: wgpu::TextureView,
    pub size: [u32; 2],
    uniform: wgpu::Buffer,
    /// the display's bind group for the cube of that generation
    group: Option<(u64, wgpu::BindGroup)>,
}

pub struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    display: wgpu::RenderPipeline,
    display_layout: wgpu::BindGroupLayout,
    stats: wgpu::ComputePipeline,
    stats_layout: wgpu::BindGroupLayout,
    cmap_texture: wgpu::Texture,
    cmap_sampler: wgpu::Sampler,
    colormap: Option<usize>,
    cube: Option<GpuCube>,
    generation: u64,
    targets: HashMap<i32, Target>,
}

fn tex_entry(
    binding: u32,
    stages: wgpu::ShaderStages,
    dim: wgpu::TextureViewDimension,
    filterable: bool,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: stages,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable },
            view_dimension: dim,
            multisampled: false,
        },
        count: None,
    }
}

fn buffer_entry(binding: u32, stages: wgpu::ShaderStages, ty: wgpu::BufferBindingType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: stages,
        ty: wgpu::BindingType::Buffer { ty, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    }
}

fn storage_entry(binding: u32, format: wgpu::TextureFormat) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format,
            view_dimension: wgpu::TextureViewDimension::D2,
        },
        count: None,
    }
}

/// The limits the map needs from the adapter: its largest textures and as many dates as it allows (wgpu's default
/// of 256 array layers is below a dense series' dates).
pub fn required_limits(adapter: &wgpu::Adapter) -> wgpu::Limits {
    let a = adapter.limits();
    wgpu::Limits {
        max_texture_dimension_2d: a.max_texture_dimension_2d.min(16384),
        max_texture_array_layers: a.max_texture_array_layers,
        max_storage_buffer_binding_size: a.max_storage_buffer_binding_size,
        max_buffer_size: a.max_buffer_size,
        ..wgpu::Limits::default()
    }
}

impl Gpu {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let vf = wgpu::ShaderStages::VERTEX_FRAGMENT;
        let frag = wgpu::ShaderStages::FRAGMENT;
        let d2 = wgpu::TextureViewDimension::D2;
        let display_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("display"),
            entries: &[
                buffer_entry(0, vf, wgpu::BufferBindingType::Uniform),
                tex_entry(1, frag, wgpu::TextureViewDimension::D2Array, false),
                tex_entry(2, frag, d2, false),
                tex_entry(3, frag, d2, false),
                tex_entry(4, frag, d2, false),
                tex_entry(5, frag, d2, true),
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: frag,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                buffer_entry(7, frag, wgpu::BufferBindingType::Storage { read_only: true }),
            ],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("display"),
            source: wgpu::ShaderSource::Wgsl(include_str!("display.wgsl").into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("display"),
            bind_group_layouts: &[Some(&display_layout)],
            immediate_size: 0,
        });
        let display = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("display"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleStrip, ..Default::default() },
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: TARGET_FORMAT,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let stats_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("stats"),
            entries: &[
                tex_entry(0, wgpu::ShaderStages::COMPUTE, wgpu::TextureViewDimension::D2Array, false),
                buffer_entry(1, wgpu::ShaderStages::COMPUTE, wgpu::BufferBindingType::Storage { read_only: true }),
                buffer_entry(2, wgpu::ShaderStages::COMPUTE, wgpu::BufferBindingType::Uniform),
                storage_entry(3, wgpu::TextureFormat::Rgba32Float),
                storage_entry(4, wgpu::TextureFormat::Rgba32Float),
                storage_entry(5, wgpu::TextureFormat::R32Float),
            ],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("stats"),
            source: wgpu::ShaderSource::Wgsl(include_str!("stats.wgsl").into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("stats"),
            bind_group_layouts: &[Some(&stats_layout)],
            immediate_size: 0,
        });
        let stats = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("stats"),
            layout: Some(&layout),
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });

        let cmap_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("colormap"),
            size: wgpu::Extent3d { width: 256, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let cmap_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("colormap"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let mut gpu = Gpu {
            device: device.clone(),
            queue: queue.clone(),
            display,
            display_layout,
            stats,
            stats_layout,
            cmap_texture,
            cmap_sampler,
            colormap: None,
            cube: None,
            generation: 0,
            targets: HashMap::new(),
        };
        gpu.set_colormap(colormaps::VIRIDIS);
        gpu
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn set_colormap(&mut self, cmap: usize) {
        if self.colormap == Some(cmap) {
            return;
        }
        self.colormap = Some(cmap);
        let lut = colormaps::all()[cmap].lut();
        self.queue.write_texture(
            self.cmap_texture.as_image_copy(),
            bytemuck::cast_slice(&lut),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(256 * 4), rows_per_image: None },
            wgpu::Extent3d { width: 256, height: 1, depth_or_array_layers: 1 },
        );
    }

    /// A new cube of w × h × t on the GPU (the dates sent with `upload_layer`), with every date in the season.
    pub fn create_cube(&mut self, w: u32, h: u32, t: u32, times: &[f32]) {
        let d = &self.device;
        let texture = d.create_texture(&wgpu::TextureDescriptor {
            label: Some("cube"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: t },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let stat_tex = |label, format| {
            d.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        };
        let stats = [
            stat_tex("stats0", wgpu::TextureFormat::Rgba32Float),
            stat_tex("stats1", wgpu::TextureFormat::Rgba32Float),
            stat_tex("stats2", wgpu::TextureFormat::R32Float),
        ];
        let times = d.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("times"),
            contents: bytemuck::cast_slice(times),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });
        let stats_uniform = d.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("stats params"),
            contents: bytemuck::bytes_of(&StatsUniform { t, nan_bits: NAN_BITS, _pad: [0; 2] }),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let stat_views = stats.each_ref().map(|s| s.create_view(&Default::default()));
        let stats_group = d.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("stats"),
            layout: &self.stats_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 1, resource: times.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: stats_uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&stat_views[0]) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&stat_views[1]) },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&stat_views[2]) },
            ],
        });
        self.generation += 1;
        self.cube = Some(GpuCube {
            w,
            h,
            t,
            texture,
            stats,
            view,
            stat_views,
            times,
            stats_group,
            generation: self.generation,
        });
    }

    fn display_group(&self, uniform: &wgpu::Buffer, c: &GpuCube) -> wgpu::BindGroup {
        let cmap_view = self.cmap_texture.create_view(&Default::default());
        let tex = wgpu::BindingResource::TextureView;
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("display"),
            layout: &self.display_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: tex(&c.view) },
                wgpu::BindGroupEntry { binding: 2, resource: tex(&c.stat_views[0]) },
                wgpu::BindGroupEntry { binding: 3, resource: tex(&c.stat_views[1]) },
                wgpu::BindGroupEntry { binding: 4, resource: tex(&c.stat_views[2]) },
                wgpu::BindGroupEntry { binding: 5, resource: tex(&cmap_view) },
                wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::Sampler(&self.cmap_sampler) },
                wgpu::BindGroupEntry { binding: 7, resource: c.times.as_entire_binding() },
            ],
        })
    }

    /// Forgets the cube (another series is being opened).
    pub fn clear_cube(&mut self) {
        self.cube = None;
    }

    pub fn has_cube(&self) -> bool {
        self.cube.is_some()
    }

    /// Sends date `i` of the overview (w × h floats) to the GPU.
    pub fn upload_layer(&self, i: u32, data: &[f32]) {
        let Some(c) = &self.cube else { return };
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &c.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y: 0, z: i },
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(data),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(c.w * 4), rows_per_image: Some(c.h) },
            wgpu::Extent3d { width: c.w, height: c.h, depth_or_array_layers: 1 },
        );
    }

    /// The dates' times (NaN = outside the season): the statistics need computing again after.
    pub fn set_times(&self, times: &[f32]) {
        if let Some(c) = &self.cube {
            self.queue.write_buffer(&c.times, 0, bytemuck::cast_slice(times));
        }
    }

    /// Computes the statistics of every pixel and reads them back, waiting for the GPU.
    pub fn compute_stats(&self) -> Option<Stats> {
        self.compute_stats_async(|| {}).rx.recv().ok().flatten()
    }

    /// Computes the statistics of every pixel; they are read back and copied in a thread of their own (the C++
    /// waits for them on the interface's thread), `on_done` called when they are ready.
    pub fn compute_stats_async(&self, on_done: impl FnOnce() + Send + 'static) -> StatsJob {
        let (tx, rx) = std::sync::mpsc::channel();
        let Some(c) = self.cube.as_ref() else {
            let _ = tx.send(None);
            return StatsJob { rx };
        };
        let start = Instant::now();
        let mut enc = self.device.create_command_encoder(&Default::default());
        {
            let mut pass =
                enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("stats"), timestamp_writes: None });
            pass.set_pipeline(&self.stats);
            pass.set_bind_group(0, &c.stats_group, &[]);
            pass.dispatch_workgroups(c.w.div_ceil(8), c.h.div_ceil(8), 1);
        }
        // read back, rows padded to 256 bytes
        let readback = |tex: &wgpu::Texture, bpp: u32, enc: &mut wgpu::CommandEncoder| {
            let row = (c.w * bpp).div_ceil(256) * 256;
            let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("stats readback"),
                size: u64::from(row) * u64::from(c.h),
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            enc.copy_texture_to_buffer(
                tex.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &buf,
                    layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: None },
                },
                wgpu::Extent3d { width: c.w, height: c.h, depth_or_array_layers: 1 },
            );
            (buf, row)
        };
        let bufs = [
            readback(&c.stats[0], 16, &mut enc),
            readback(&c.stats[1], 16, &mut enc),
            readback(&c.stats[2], 4, &mut enc),
        ];
        let index = self.queue.submit([enc.finish()]);
        for (b, _) in &bufs {
            b.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        }
        let device = self.device.clone();
        let (w, h) = (c.w as usize, c.h as usize);
        std::thread::Builder::new()
            .name("statistics".into())
            .spawn(move || {
                let stats = (|| {
                    device.poll(wgpu::PollType::Wait { submission_index: Some(index), timeout: None }).ok()?;
                    let gpu_ms = start.elapsed().as_secs_f64() * 1000.0;
                    let s0 = unpad::<[f32; 4]>(&bufs[0], w, h)?;
                    let s1 = unpad::<[f32; 4]>(&bufs[1], w, h)?;
                    let s2 = unpad::<f32>(&bufs[2], w, h)?;
                    Some(Stats { w, h, s0, s1, s2, ms: start.elapsed().as_secs_f64() * 1000.0, gpu_ms })
                })();
                let _ = tx.send(stats);
                on_done();
            })
            .expect("thread");
        StatsJob { rx }
    }

    fn target_uniform(device: &wgpu::Device) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("view"),
            size: std::mem::size_of::<ViewUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    /// The target of a map panel (`slot`), made or remade at the size asked. Returns whether it is new (its
    /// texture must then be registered with egui again).
    pub fn ensure_target(&mut self, slot: i32, size: [u32; 2]) -> bool {
        let size = [size[0].max(1), size[1].max(1)];
        if self.targets.get(&slot).is_some_and(|t| t.size == size) {
            return false;
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("map target"),
            size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TARGET_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let uniform = Self::target_uniform(&self.device);
        self.targets.insert(slot, Target { texture, view, size, uniform, group: None });
        true
    }

    pub fn target(&self, slot: i32) -> Option<&Target> {
        self.targets.get(&slot)
    }

    /// Draws the cube into a target, cleared first. Without a cube the target is only cleared.
    pub fn render(&mut self, slot: i32, p: Option<&DrawParams>) {
        let mut draw = false;
        if let (Some(p), Some(c)) = (p, &self.cube)
            && let Some(target) = self.targets.get(&slot)
        {
            let [w, h] = target.size.map(|v| v as f32);
            let ndc = |x: f32, y: f32| [x / w * 2.0 - 1.0, 1.0 - y / h * 2.0];
            let [x0, y0] = ndc(p.rect[0], p.rect[1]);
            let [x1, y1] = ndc(p.rect[2], p.rect[3]);
            let u = ViewUniform {
                rect: [x0, y0, x1, y1],
                layers: [p.layers[0], p.layers[1], p.layers[2], 0],
                range: p.range,
                alpha: p.alpha,
                mode: p.mode as i32,
                t: c.t,
                nan_bits: NAN_BITS,
                _pad: [0; 2],
            };
            self.queue.write_buffer(&target.uniform, 0, bytemuck::bytes_of(&u));
            if target.group.as_ref().is_none_or(|g| g.0 != c.generation) {
                let group = self.display_group(&target.uniform, c);
                let generation = c.generation;
                self.targets.get_mut(&slot).unwrap().group = Some((generation, group));
            }
            draw = true;
        }
        let Some(target) = self.targets.get(&slot) else { return };
        let mut enc = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("map"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if let (true, Some((_, g))) = (draw, &target.group) {
                pass.set_pipeline(&self.display);
                pass.set_bind_group(0, g, &[]);
                pass.draw(0..4, 0..1);
            }
        }
        self.queue.submit([enc.finish()]);
    }

    /// The target's pixels (RGBA, rows from the top), for tests and exports.
    pub fn read_target(&self, slot: i32) -> Option<(u32, u32, Vec<u8>)> {
        let t = self.targets.get(&slot)?;
        let [w, h] = t.size;
        let row = (w * 4).div_ceil(256) * 256;
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("target readback"),
            size: u64::from(row) * u64::from(h),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            t.texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: None },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.queue.submit([enc.finish()]);
        buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        self.device.poll(wgpu::PollType::wait_indefinitely()).ok()?;
        let Ok(data) = buf.slice(..).get_mapped_range() else { return None };
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h as usize {
            out.extend_from_slice(&data[y * row as usize..y * row as usize + w as usize * 4]);
        }
        Some((w, h, out))
    }
}
