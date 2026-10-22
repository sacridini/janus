//! The map on the GPU with wgpu (src/gpu.hpp, gpu_gl.cpp, gpu_metal.mm). Each open series has its cube
//! ([`GpuCube`]: the overview as an R32F texture array, its per-pixel temporal statistics computed in one compute
//! pass); a map panel is drawn into an offscreen target of its own, a frame at a time ([`Gpu::begin_map`]): cubes
//! and detail tiles through a colormap (or a table of class colours), RGBA images (basemap tiles, composites) and
//! plain rectangles, each with an opacity, straight or through a warp grid (a layer in another CRS). One shader
//! source (WGSL) for every system.

pub mod colormaps;

pub mod vector;
pub use vector::{VectorFeatures, VectorGeometry, VectorKind, VectorPoint, VectorStyle, VectorView};

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
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

/// A texture on the GPU (a detail tile, an image, a class table, a warp grid), shared by whoever draws it.
pub struct Texture {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub width: u32,
    pub height: u32,
}

pub type GpuTexture = Arc<Texture>;

impl Texture {
    /// Bytes on the GPU.
    pub fn bytes(&self) -> usize {
        let b = self.texture.format().block_copy_size(None).unwrap_or(4) as usize;
        self.width as usize * self.height as usize * b
    }
}

/// A layer in another CRS than the map's: each point of the quad goes to the layer's own coordinates through a
/// grid made on the CPU (the layer's uv at nodes over a map-space domain), interpolated bilinearly; points outside
/// the drawn texture are discarded (gpu.hpp, WarpParams).
#[derive(Clone)]
pub struct Warp {
    pub grid: GpuTexture,
    /// the quad in the grid's domain (0..1): x0, y0, w, h
    pub quad: [f32; 4],
    /// the drawn texture (cube, tile) in the layer's uv: x0, y0, w, h
    pub src: [f32; 4],
}

/// How a layer is drawn (gpu.hpp, DrawParams).
#[derive(Clone)]
pub struct LayerParams {
    pub mode: Mode,
    /// the date (Value, Anomaly), the date and its reference (Diff), three dates (Rgb), or the composite's first
    /// and last date and reducer
    pub layers: [i32; 3],
    pub range: [f32; 2],
    /// categorical data: class value -> colour (see Gpu::create_class_lut)
    pub class_lut: Option<GpuTexture>,
    /// Diff on a detail tile: the reference date's tile
    pub tile2: Option<GpuTexture>,
    pub warp: Option<Warp>,
}

impl LayerParams {
    pub fn new(mode: Mode, layers: [i32; 3], range: [f32; 2]) -> Self {
        LayerParams { mode, layers, range, class_lut: None, tile2: None, warp: None }
    }
}

/// Class colours of categorical data: CLASS_LUT_SIZE texels, value v -> texel v (alpha 0 = not a class of the
/// series, drawn transparent).
pub const CLASS_LUT_SIZE: u32 = 4096;

#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct DrawUniform {
    rect: [f32; 4],
    layers: [i32; 4],
    range: [f32; 2],
    alpha: f32,
    mode: i32,
    source: i32,
    classes: i32,
    warp: i32,
    nearest: i32,
    nan_bits: u32,
    _pad: [u32; 3],
    color: [f32; 4],
    warp_quad: [f32; 4],
    warp_src: [f32; 4],
}

/// Each draw's uniforms sit at a multiple of this in one buffer (WebGPU's minimum uniform offset alignment).
const UNIFORM_STRIDE: u64 = 256;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct StatsUniform {
    t: u32,
    nan_bits: u32,
    _pad: [u32; 2],
}

const NAN_BITS: u32 = 0x7fc0_0000;

/// The per-pixel statistics read back to the CPU (for the ranges, histograms and the Statistics panel).
#[derive(Default)]
pub struct Stats {
    pub w: usize,
    pub h: usize,
    /// mean, std, slope, n per pixel
    pub s0: Vec<[f32; 4]>,
    /// min, max, R², largest drop per pixel
    pub s1: Vec<[f32; 4]>,
    /// date index of the largest drop
    pub s2: Vec<f32>,
    /// the whole time (ms): compute, readback and copy
    pub ms: f64,
    /// until the GPU had computed and copied them to readable memory (ms)
    pub gpu_ms: f64,
}

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

    /// Waits for them.
    pub fn wait(self) -> Option<Stats> {
        self.rx.recv().ok().flatten()
    }
}

/// A series' cube on the GPU: the overview's dates, their times (years since the first; NaN = outside the season)
/// and the statistics.
pub struct GpuCube {
    pub w: u32,
    pub h: u32,
    pub t: u32,
    queue: wgpu::Queue,
    texture: wgpu::Texture,
    stats: [wgpu::Texture; 3],
    times: wgpu::Buffer,
    /// the display's group 1
    group: wgpu::BindGroup,
    stats_group: wgpu::BindGroup,
    loaded: Vec<bool>,
}

impl GpuCube {
    /// Sends date `i` of the overview (w × h floats).
    pub fn upload_layer(&mut self, i: u32, data: &[f32]) {
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y: 0, z: i },
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(data),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(self.w * 4), rows_per_image: Some(self.h) },
            wgpu::Extent3d { width: self.w, height: self.h, depth_or_array_layers: 1 },
        );
        self.loaded[i as usize] = true;
    }

    pub fn loaded(&self, i: usize) -> bool {
        self.loaded.get(i).copied().unwrap_or(false)
    }

    /// The dates' times (years since the first; NaN = outside the season): the statistics need computing again.
    pub fn set_times(&self, times: &[f32]) {
        self.queue.write_buffer(&self.times, 0, bytemuck::cast_slice(times));
    }

    /// Bytes on the GPU: the dates and the statistics (w × h × (T + 9) floats).
    pub fn bytes(&self) -> usize {
        self.w as usize * self.h as usize * (self.t as usize + 9) * 4
    }
}

/// An offscreen target (a map panel): an RGBA texture the size of the panel in pixels, holding the sRGB bytes as
/// GL's framebuffer holds them (egui samples textures as these bytes: it works in gamma space).
pub struct Target {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub size: [u32; 2],
}

/// One draw of a map frame.
struct PendingDraw {
    uniform: DrawUniform,
    cube_group: wgpu::BindGroup,
    textures: wgpu::BindGroup,
    /// the scissor rectangle (x, y, w, h in the target's pixels), None: the whole target
    clip: Option<[u32; 4]>,
}

/// A map frame being drawn into a target (Gpu::begin_map ... Gpu::end_map).
pub struct MapFrame {
    slot: i32,
    /// the target's size in pixels
    pub size: [u32; 2],
    clear: [f32; 4],
    draws: Vec<PendingDraw>,
    /// the vector layers' draws, after the rasters
    vdraws: Vec<vector::VectorDraw>,
    /// the draws pushed while set are cut to this rectangle (x, y, w, h in the target's pixels)
    pub clip: Option<[u32; 4]>,
}

pub struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    display: wgpu::RenderPipeline,
    uniform_layout: wgpu::BindGroupLayout,
    cube_layout: wgpu::BindGroupLayout,
    texture_layout: wgpu::BindGroupLayout,
    stats: wgpu::ComputePipeline,
    stats_layout: wgpu::BindGroupLayout,
    linear_sampler: wgpu::Sampler,
    nearest_sampler: wgpu::Sampler,
    colormaps: RefCell<HashMap<usize, GpuTexture>>,
    /// the cube group of draws without a cube (images, rectangles, overlays): a 1 x 1 x 1 cube
    no_cube: Option<wgpu::BindGroup>,
    dummy_r32: GpuTexture,
    dummy_rgba: GpuTexture,
    dummy_rg32: GpuTexture,
    uniforms: RefCell<(wgpu::Buffer, u64)>,
    targets: HashMap<i32, Target>,
    vector: vector::VectorPipelines,
}

/// The targets' format: the sRGB bytes as they are, blended as they are (GL's blending of the C++).
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

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

fn buffer_entry(
    binding: u32,
    stages: wgpu::ShaderStages,
    ty: wgpu::BufferBindingType,
    dynamic: bool,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: stages,
        ty: wgpu::BindingType::Buffer { ty, has_dynamic_offset: dynamic, min_binding_size: None },
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
        let d = device;
        let vf = wgpu::ShaderStages::VERTEX_FRAGMENT;
        let frag = wgpu::ShaderStages::FRAGMENT;
        let d2 = wgpu::TextureViewDimension::D2;
        let uniform_layout = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("draw"),
            entries: &[buffer_entry(0, vf, wgpu::BufferBindingType::Uniform, true)],
        });
        let cube_layout = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("cube"),
            entries: &[
                tex_entry(0, frag, wgpu::TextureViewDimension::D2Array, false),
                tex_entry(1, frag, d2, false),
                tex_entry(2, frag, d2, false),
                tex_entry(3, frag, d2, false),
                buffer_entry(4, frag, wgpu::BufferBindingType::Storage { read_only: true }, false),
            ],
        });
        let sampler_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: frag,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };
        let texture_layout = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("textures"),
            entries: &[
                tex_entry(0, frag, d2, true),
                sampler_entry(1),
                sampler_entry(2),
                tex_entry(3, frag, d2, false),
                tex_entry(4, frag, d2, false),
                tex_entry(5, frag, d2, true),
                tex_entry(6, frag, d2, false),
                tex_entry(7, frag, d2, false),
            ],
        });
        let module = d.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("display"),
            source: wgpu::ShaderSource::Wgsl(include_str!("display.wgsl").into()),
        });
        let layout = d.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("display"),
            bind_group_layouts: &[Some(&uniform_layout), Some(&cube_layout), Some(&texture_layout)],
            immediate_size: 0,
        });
        let display = d.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
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
                // SRC_ALPHA, ONE_MINUS_SRC_ALPHA (gpu_gl.cpp); an opaque draw (alpha 1) replaces
                targets: &[Some(wgpu::ColorTargetState {
                    format: TARGET_FORMAT,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let stats_layout = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("stats"),
            entries: &[
                tex_entry(0, wgpu::ShaderStages::COMPUTE, wgpu::TextureViewDimension::D2Array, false),
                buffer_entry(
                    1,
                    wgpu::ShaderStages::COMPUTE,
                    wgpu::BufferBindingType::Storage { read_only: true },
                    false,
                ),
                buffer_entry(2, wgpu::ShaderStages::COMPUTE, wgpu::BufferBindingType::Uniform, false),
                storage_entry(3, wgpu::TextureFormat::Rgba32Float),
                storage_entry(4, wgpu::TextureFormat::Rgba32Float),
                storage_entry(5, wgpu::TextureFormat::R32Float),
            ],
        });
        let module = d.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("stats"),
            source: wgpu::ShaderSource::Wgsl(include_str!("stats.wgsl").into()),
        });
        let layout = d.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("stats"),
            bind_group_layouts: &[Some(&stats_layout)],
            immediate_size: 0,
        });
        let stats = d.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("stats"),
            layout: Some(&layout),
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let sampler = |filter| {
            d.create_sampler(&wgpu::SamplerDescriptor {
                mag_filter: filter,
                min_filter: filter,
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                ..Default::default()
            })
        };
        let make = |format, w, h, bytes: &[u8]| {
            let texture = d.create_texture_with_data(
                queue,
                &wgpu::TextureDescriptor {
                    label: Some("placeholder"),
                    size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
                wgpu::util::TextureDataOrder::LayerMajor,
                bytes,
            );
            let view = texture.create_view(&Default::default());
            Arc::new(Texture { texture, view, width: w, height: h })
        };
        let dummy_r32 = make(wgpu::TextureFormat::R32Float, 1, 1, &[0; 4]);
        let dummy_rgba = make(wgpu::TextureFormat::Rgba8Unorm, 1, 1, &[0; 4]);
        let dummy_rg32 = make(wgpu::TextureFormat::Rg32Float, 2, 2, &[0; 32]);
        let uniform_layout_for_vectors = uniform_layout.clone();
        let uniforms = d.create_buffer(&wgpu::BufferDescriptor {
            label: Some("draws"),
            size: UNIFORM_STRIDE * 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut gpu = Gpu {
            device: d.clone(),
            queue: queue.clone(),
            display,
            uniform_layout,
            cube_layout,
            texture_layout,
            stats,
            stats_layout,
            linear_sampler: sampler(wgpu::FilterMode::Linear),
            nearest_sampler: sampler(wgpu::FilterMode::Nearest),
            colormaps: RefCell::new(HashMap::new()),
            no_cube: None,
            dummy_r32,
            dummy_rgba,
            dummy_rg32,
            uniforms: RefCell::new((uniforms, 64)),
            targets: HashMap::new(),
            vector: vector::VectorPipelines::new(d, &uniform_layout_for_vectors),
        };
        let empty = gpu.create_cube(1, 1, 1, &[0.0]);
        gpu.no_cube = Some(empty.group.clone());
        gpu
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    fn no_cube(&self) -> wgpu::BindGroup {
        self.no_cube.clone().expect("made in new")
    }

    /// The colormap's lookup texture (256 colours, bilinear), made once.
    fn colormap(&self, cmap: usize) -> GpuTexture {
        if let Some(t) = self.colormaps.borrow().get(&cmap) {
            return t.clone();
        }
        let lut = colormaps::all()[cmap.min(colormaps::all().len() - 1)].lut();
        let t = self.create_texture(wgpu::TextureFormat::Rgba8Unorm, 256, 1, bytemuck::cast_slice(&lut));
        self.colormaps.borrow_mut().insert(cmap, t.clone());
        t
    }

    fn create_texture(&self, format: wgpu::TextureFormat, w: u32, h: u32, bytes: &[u8]) -> GpuTexture {
        let texture = self.device.create_texture_with_data(
            &self.queue,
            &wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            bytes,
        );
        let view = texture.create_view(&Default::default());
        Arc::new(Texture { texture, view, width: w, height: h })
    }

    /// A detail tile: w × h floats.
    pub fn create_tile(&self, w: u32, h: u32, data: &[f32]) -> GpuTexture {
        self.create_texture(wgpu::TextureFormat::R32Float, w, h, bytemuck::cast_slice(data))
    }

    /// An RGBA image (rows from the top): a basemap tile, a composite.
    pub fn create_image(&self, w: u32, h: u32, rgba: &[u8]) -> GpuTexture {
        self.create_texture(wgpu::TextureFormat::Rgba8Unorm, w, h, rgba)
    }

    /// A table of class colours: CLASS_LUT_SIZE RGBA texels.
    pub fn create_class_lut(&self, rgba: &[u8]) -> GpuTexture {
        self.create_texture(wgpu::TextureFormat::Rgba8Unorm, CLASS_LUT_SIZE, 1, rgba)
    }

    /// A warp grid: w × h (u, v) pairs, rows from the top.
    pub fn create_warp_grid(&self, w: u32, h: u32, rg: &[f32]) -> GpuTexture {
        self.create_texture(wgpu::TextureFormat::Rg32Float, w, h, bytemuck::cast_slice(rg))
    }

    /// A new cube of w × h × t on the GPU (the dates sent with upload_layer), `times` the dates in years since the
    /// first (NaN = outside the season).
    pub fn create_cube(&self, w: u32, h: u32, t: u32, times: &[f32]) -> GpuCube {
        let d = &self.device;
        // two layers at least: wgpu's OpenGL makes a texture of one layer a plain 2D texture, which cannot be
        // viewed as an array
        let texture = d.create_texture(&wgpu::TextureDescriptor {
            label: Some("cube"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: t.max(2) },
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
        let sv = stats.each_ref().map(|s| s.create_view(&Default::default()));
        let tv = wgpu::BindingResource::TextureView;
        let group = d.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("cube"),
            layout: &self.cube_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: tv(&view) },
                wgpu::BindGroupEntry { binding: 1, resource: tv(&sv[0]) },
                wgpu::BindGroupEntry { binding: 2, resource: tv(&sv[1]) },
                wgpu::BindGroupEntry { binding: 3, resource: tv(&sv[2]) },
                wgpu::BindGroupEntry { binding: 4, resource: times.as_entire_binding() },
            ],
        });
        let stats_group = d.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("stats"),
            layout: &self.stats_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: tv(&view) },
                wgpu::BindGroupEntry { binding: 1, resource: times.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: stats_uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: tv(&sv[0]) },
                wgpu::BindGroupEntry { binding: 4, resource: tv(&sv[1]) },
                wgpu::BindGroupEntry { binding: 5, resource: tv(&sv[2]) },
            ],
        });
        GpuCube {
            w,
            h,
            t,
            queue: self.queue.clone(),
            texture,
            stats,
            times,
            group,
            stats_group,
            loaded: vec![false; t as usize],
        }
    }

    /// Computes the statistics of every pixel; they are read back and copied in a thread of their own (the C++
    /// waits for them on the interface's thread), `on_done` called when they are ready.
    pub fn compute_stats_async(&self, c: &GpuCube, on_done: impl FnOnce() + Send + 'static) -> StatsJob {
        let (tx, rx) = std::sync::mpsc::channel();
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
        self.targets.insert(slot, Target { texture, view, size });
        true
    }

    pub fn target(&self, slot: i32) -> Option<&Target> {
        self.targets.get(&slot)
    }

    /// A closed panel's target.
    pub fn release_target(&mut self, slot: i32) {
        self.targets.remove(&slot);
    }

    /// Starts a frame of the map in a target of `size` pixels (made if needed), cleared to `bg` (sRGB).
    pub fn begin_map(&mut self, slot: i32, size: [u32; 2], bg: [f32; 4]) -> MapFrame {
        self.ensure_target(slot, size);
        MapFrame {
            slot,
            size: [size[0].max(1), size[1].max(1)],
            clear: bg,
            draws: Vec::new(),
            vdraws: Vec::new(),
            clip: None,
        }
    }

    fn uniform(&self, f: &MapFrame, rect: [f32; 4]) -> DrawUniform {
        let [w, h] = f.size.map(|v| v as f32);
        DrawUniform {
            rect: [rect[0] / w * 2.0 - 1.0, 1.0 - rect[1] / h * 2.0, rect[2] / w * 2.0 - 1.0, 1.0 - rect[3] / h * 2.0],
            alpha: 1.0,
            nan_bits: NAN_BITS,
            ..Default::default()
        }
    }

    fn textures(
        &self,
        cmap: Option<usize>,
        tile: Option<&GpuTexture>,
        image: Option<(&GpuTexture, bool)>,
        p: Option<&LayerParams>,
    ) -> wgpu::BindGroup {
        let cmap = cmap.map(|c| self.colormap(c));
        fn tv<'a>(t: Option<&'a GpuTexture>, dummy: &'a GpuTexture) -> wgpu::BindingResource<'a> {
            wgpu::BindingResource::TextureView(&t.unwrap_or(dummy).view)
        }
        let image_sampler = if image.is_some_and(|i| i.1) { &self.nearest_sampler } else { &self.linear_sampler };
        let (tile2, lut, warp) = match p {
            Some(p) => (p.tile2.as_ref(), p.class_lut.as_ref(), p.warp.as_ref().map(|w| &w.grid)),
            None => (None, None, None),
        };
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("textures"),
            layout: &self.texture_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: tv(cmap.as_ref(), &self.dummy_rgba) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.linear_sampler) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(image_sampler) },
                wgpu::BindGroupEntry { binding: 3, resource: tv(tile, &self.dummy_r32) },
                wgpu::BindGroupEntry { binding: 4, resource: tv(tile2, &self.dummy_r32) },
                wgpu::BindGroupEntry { binding: 5, resource: tv(image.map(|i| i.0), &self.dummy_rgba) },
                wgpu::BindGroupEntry { binding: 6, resource: tv(lut, &self.dummy_rgba) },
                wgpu::BindGroupEntry { binding: 7, resource: tv(warp, &self.dummy_rg32) },
            ],
        })
    }

    fn layer_uniform(&self, f: &MapFrame, rect: [f32; 4], p: &LayerParams, source: i32, alpha: f32) -> DrawUniform {
        let mut u = self.uniform(f, rect);
        let hi = if p.range[1] > p.range[0] { p.range[1] } else { p.range[0] + 1e-6 };
        u.layers = [p.layers[0], p.layers[1], p.layers[2], 0];
        u.range = [p.range[0], hi];
        u.alpha = alpha;
        u.mode = p.mode as i32;
        u.source = source;
        u.classes = i32::from(p.class_lut.is_some());
        if let Some(w) = &p.warp {
            u.warp = 1;
            u.warp_quad = w.quad;
            u.warp_src = w.src;
        }
        u
    }

    /// A cube through colormap `cmap` with an opacity; `rect` its place in the target's pixels (x0, y0, x1, y1).
    pub fn draw_cube(&self, f: &mut MapFrame, c: &GpuCube, rect: [f32; 4], p: &LayerParams, cmap: usize, alpha: f32) {
        let uniform = self.layer_uniform(f, rect, p, 0, alpha);
        let textures = self.textures(Some(cmap), None, None, Some(p));
        f.draws.push(PendingDraw { uniform, cube_group: c.group.clone(), textures, clip: f.clip });
    }

    /// A detail tile (with the cube's statistics for the modes that need them).
    #[allow(clippy::too_many_arguments)]
    pub fn draw_tile(
        &self,
        f: &mut MapFrame,
        c: &GpuCube,
        tile: &GpuTexture,
        rect: [f32; 4],
        p: &LayerParams,
        cmap: usize,
        alpha: f32,
    ) {
        let uniform = self.layer_uniform(f, rect, p, 1, alpha);
        let textures = self.textures(Some(cmap), Some(tile), None, Some(p));
        f.draws.push(PendingDraw { uniform, cube_group: c.group.clone(), textures, clip: f.clip });
    }

    /// A single-band raster (e.g. a Zeit result) through its own colormap and range.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_overlay(
        &self,
        f: &mut MapFrame,
        tex: &GpuTexture,
        rect: [f32; 4],
        range: [f32; 2],
        cmap: usize,
        alpha: f32,
        warp: Option<&Warp>,
    ) {
        let mut p = LayerParams::new(Mode::Value, [0, 0, 0], range);
        p.warp = warp.cloned();
        self.draw_values(f, tex, rect, &p, cmap, alpha);
    }

    /// A float texture of values that belongs to no cube (a result map, a transect's image), drawn as a detail
    /// tile in `p`'s range and class colours.
    pub fn draw_values(
        &self,
        f: &mut MapFrame,
        tex: &GpuTexture,
        rect: [f32; 4],
        p: &LayerParams,
        cmap: usize,
        alpha: f32,
    ) {
        let uniform = self.layer_uniform(f, rect, p, 1, alpha);
        let textures = self.textures(Some(cmap), Some(tex), None, Some(p));
        f.draws.push(PendingDraw { uniform, cube_group: self.no_cube(), textures, clip: f.clip });
    }

    /// An RGBA image blended by its own alpha × `alpha`, bilinear (or `nearest`), through a warp if given.
    pub fn draw_image(
        &self,
        f: &mut MapFrame,
        tex: &GpuTexture,
        rect: [f32; 4],
        alpha: f32,
        warp: Option<&Warp>,
        nearest: bool,
    ) {
        let mut u = self.uniform(f, rect);
        u.source = 2;
        u.alpha = alpha;
        u.nearest = i32::from(nearest);
        let mut p = LayerParams::new(Mode::Value, [0, 0, 0], [0.0, 1.0]);
        if let Some(w) = warp {
            u.warp = 1;
            u.warp_quad = w.quad;
            u.warp_src = w.src;
            p.warp = Some(w.clone());
        }
        let textures = self.textures(None, None, Some((tex, nearest)), Some(&p));
        f.draws.push(PendingDraw { uniform: u, cube_group: self.no_cube(), textures, clip: f.clip });
    }

    /// A rectangle of one colour (sRGB, alpha its opacity) blended over the map.
    pub fn fill_rect(&self, f: &mut MapFrame, rect: [f32; 4], rgba: [f32; 4]) {
        let mut u = self.uniform(f, rect);
        u.source = 3;
        u.color = rgba;
        let textures = self.textures(None, None, None, None);
        f.draws.push(PendingDraw { uniform: u, cube_group: self.no_cube(), textures, clip: f.clip });
    }

    /// Draws the frame's draws in order, in one pass.
    pub fn end_map(&self, f: MapFrame) {
        let Some(target) = self.targets.get(&f.slot) else { return };
        let n = f.draws.len() as u64;
        {
            let mut u = self.uniforms.borrow_mut();
            if u.1 < n {
                let cap = n.next_power_of_two();
                u.0 = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("draws"),
                    size: UNIFORM_STRIDE * cap,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                u.1 = cap;
            }
            if n > 0 {
                let mut bytes = vec![0u8; (UNIFORM_STRIDE * n) as usize];
                for (i, d) in f.draws.iter().enumerate() {
                    let at = i * UNIFORM_STRIDE as usize;
                    bytes[at..at + size_of::<DrawUniform>()].copy_from_slice(bytemuck::bytes_of(&d.uniform));
                }
                self.queue.write_buffer(&u.0, 0, &bytes);
            }
        }
        let u = self.uniforms.borrow();
        let uniform_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("draws"),
            layout: &self.uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &u.0,
                    offset: 0,
                    size: wgpu::BufferSize::new(size_of::<DrawUniform>() as u64),
                }),
            }],
        });
        let rgb = [f.clear[0], f.clear[1], f.clear[2]];
        let mut enc = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("map"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: f64::from(rgb[0]),
                            g: f64::from(rgb[1]),
                            b: f64::from(rgb[2]),
                            a: f64::from(f.clear[3]),
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.display);
            let [tw, th] = target.size;
            for (i, d) in f.draws.iter().enumerate() {
                let [x, y, w, h] = d.clip.unwrap_or([0, 0, tw, th]);
                let (x, y) = (x.min(tw), y.min(th));
                let (w, h) = (w.min(tw - x), h.min(th - y));
                if w == 0 || h == 0 {
                    continue;
                }
                pass.set_scissor_rect(x, y, w, h);
                pass.set_bind_group(0, &uniform_group, &[(i as u64 * UNIFORM_STRIDE) as u32]);
                pass.set_bind_group(1, &d.cube_group, &[]);
                pass.set_bind_group(2, &d.textures, &[]);
                pass.draw(0..4, 0..1);
            }
            self.record_vectors(&f, &mut pass, target.size);
        }
        self.queue.submit([enc.finish()]);
    }

    /// The target's pixels (sRGB bytes, rows from the top), for tests and exports.
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
        let data = buf.slice(..).get_mapped_range().ok()?;
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h as usize {
            out.extend_from_slice(&data[y * row as usize..y * row as usize + w as usize * 4]);
        }
        Some((w, h, out))
    }
}

/// The parts of `full` (x0, y0, x1, y1 in a target's pixels) that none of `holes` covers, as scissor rectangles
/// (x, y, w, h): what is drawn under tiles drawn over it, so that a translucent layer is blended once. Edges are
/// rounded as the rasterizer covers pixels (a pixel's centre inside); rows of the same cut merge.
pub fn uncovered(full: [f32; 4], holes: &[[f32; 4]]) -> Vec<[u32; 4]> {
    let px = |v: f32| v.round().max(0.0) as u32;
    let r = |q: &[f32; 4]| [px(q[0].min(q[2])), px(q[1].min(q[3])), px(q[0].max(q[2])), px(q[1].max(q[3]))];
    let f = r(&full);
    let holes: Vec<[u32; 4]> = holes.iter().map(r).filter(|h| h[0] < h[2] && h[1] < h[3]).collect();
    let mut ys: Vec<u32> = vec![f[1], f[3]];
    for h in &holes {
        ys.extend([h[1].clamp(f[1], f[3]), h[3].clamp(f[1], f[3])]);
    }
    ys.sort_unstable();
    ys.dedup();
    let mut out: Vec<[u32; 4]> = Vec::new();
    for b in ys.windows(2) {
        let (y0, y1) = (b[0], b[1]);
        // the holes across this band, as x intervals, and the gaps between them
        let mut spans: Vec<(u32, u32)> =
            holes.iter().filter(|h| h[1] <= y0 && h[3] >= y1).map(|h| (h[0].max(f[0]), h[2].min(f[2]))).collect();
        spans.sort_unstable();
        let mut x = f[0];
        let mut gaps = Vec::new();
        for (a, e) in spans {
            if a > x {
                gaps.push((x, a));
            }
            x = x.max(e);
        }
        if x < f[2] {
            gaps.push((x, f[2]));
        }
        for (a, e) in gaps {
            // the band above with the same gap grows down
            match out.iter_mut().find(|o| o[0] == a && o[2] == e - a && o[1] + o[3] == y0) {
                Some(o) => o[3] += y1 - y0,
                None => out.push([a, y0, e - a, y1 - y0]),
            }
        }
    }
    out
}

#[cfg(test)]
mod uncovered_tests {
    use super::uncovered;

    #[test]
    fn holes_cut_out() {
        // nothing over it: the whole
        assert_eq!(uncovered([0.0, 0.0, 100.0, 50.0], &[]), vec![[0, 0, 100, 50]]);
        // a tile in the middle: four parts, their area the rest
        let parts = uncovered([0.0, 0.0, 100.0, 100.0], &[[20.0, 30.0, 60.0, 70.0]]);
        let area: u32 = parts.iter().map(|p| p[2] * p[3]).sum();
        assert_eq!(area, 100 * 100 - 40 * 40);
        // two tiles side by side covering all: nothing
        assert!(uncovered([0.0, 0.0, 100.0, 100.0], &[[0.0, 0.0, 50.4, 100.0], [50.4, 0.0, 100.0, 100.0]]).is_empty());
        // a tile past the edge
        let parts = uncovered([10.0, 10.0, 50.0, 50.0], &[[-20.0, -20.0, 30.0, 30.0]]);
        assert_eq!(parts.iter().map(|p| p[2] * p[3]).sum::<u32>(), 40 * 40 - 20 * 20);
    }
}
