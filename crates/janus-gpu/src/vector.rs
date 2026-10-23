//! Vector layers drawn by the GPU (vector.wgsl): a layer's geometry uploaded once, a buffer of points per level of
//! detail (relative to the origins of the layer's pieces, so float32 keeps its precision when zoomed in; the fills'
//! triangles as indices into them), its features' colours and pieces in buffers of their own (the colours changed
//! without uploading the geometry again); each frame only a transform and the ranges of the pieces in view. Drawn
//! into a map frame in the order pushed, between its rasters (a vector layer under a raster one, as in a GIS).

use std::ops::Range;

use wgpu::util::DeviceExt;

use crate::{Gpu, MapFrame, TARGET_FORMAT};

/// A point of a level: x, y relative to its piece's origin (source pixels; a NaN x breaks a line) and its
/// feature's index (its colour and piece).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct VectorPoint {
    pub x: f32,
    pub y: f32,
    pub f: u32,
}

impl VectorPoint {
    /// A break between two lines.
    pub const BREAK: VectorPoint = VectorPoint { x: f32::NAN, y: f32::NAN, f: 0 };
}

/// A layer's features (their colours, RGBA bytes packed little-endian, r in the low byte; their pieces) and its
/// pieces' origins.
pub struct VectorFeatures {
    colors: wgpu::Buffer,
    piece_of: wgpu::Buffer,
    origins: wgpu::Buffer,
    n: usize,
    pub bytes: usize,
}

/// A level of a layer on the GPU: its points (lines and points) and its fills' triangles.
pub struct VectorGeometry {
    group: wgpu::BindGroup,
    n: u32,
    index: Option<(wgpu::Buffer, u32)>,
    pub bytes: usize,
}

/// What a draw makes of a level.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VectorKind {
    /// its points as lines
    Lines,
    /// its triangles
    Fill,
    /// its points as squares or discs
    Points { round: bool },
}

/// How a draw looks: its core's width (lines) or diameter (points) and the rim around it (target pixels), the
/// rim's colour, an opacity over the features'.
#[derive(Clone, Copy, Debug)]
pub struct VectorStyle {
    pub width: f32,
    pub halo: f32,
    pub halo_color: [f32; 4],
    pub alpha: f32,
}

/// Where a layer is drawn: target pixel = a * (source pixel - centre) + t0.
#[derive(Clone, Copy, Debug)]
pub struct VectorView {
    pub a: [f64; 2],
    pub centre: [f64; 2],
    pub t0: [f64; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct VectorUniform {
    a: [f32; 2],
    c_hi: [f32; 2],
    c_lo: [f32; 2],
    t0: [f32; 2],
    size: [f32; 2],
    width: f32,
    halo: f32,
    halo_color: [f32; 4],
    feather: f32,
    alpha: f32,
    round: u32,
    _pad: u32,
}

/// A double as two floats (high and low parts).
pub fn split(v: f64) -> [f32; 2] {
    let hi = v as f32;
    [hi, (v - f64::from(hi)) as f32]
}

/// One vector draw of a map frame: its ranges (of points for lines and points, of indices for fills).
pub(crate) struct VectorDraw {
    uniform: VectorUniform,
    group: wgpu::BindGroup,
    kind: VectorKind,
    ranges: Vec<Range<u32>>,
    index: Option<wgpu::Buffer>,
    clip: Option<[u32; 4]>,
    /// the raster draws pushed before it: drawn after them, before the next
    pub(crate) after: usize,
}

/// The vector pipelines (lines, fills, points) and their layout.
pub(crate) struct VectorPipelines {
    layout: wgpu::BindGroupLayout,
    lines: wgpu::RenderPipeline,
    fill: wgpu::RenderPipeline,
    points: wgpu::RenderPipeline,
}

impl VectorPipelines {
    pub(crate) fn new(d: &wgpu::Device, uniform_layout: &wgpu::BindGroupLayout) -> VectorPipelines {
        let storage = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let layout = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vector"),
            entries: &[storage(0), storage(1), storage(2), storage(3)],
        });
        let module = d.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vector"),
            source: wgpu::ShaderSource::Wgsl(include_str!("vector.wgsl").into()),
        });
        let pl = d.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("vector"),
            bind_group_layouts: &[Some(uniform_layout), Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = |vs: &str, fs: &str| {
            d.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("vector"),
                layout: Some(&pl),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some(vs),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: TARGET_FORMAT,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        VectorPipelines {
            lines: pipeline("vs_line", "fs_line"),
            fill: pipeline("vs_fill", "fs_fill"),
            points: pipeline("vs_point", "fs_point"),
            layout,
        }
    }
}

impl Gpu {
    /// A layer's features on the GPU: their colours, their pieces, the pieces' origins (source pixels).
    pub fn create_vector_features(&self, colors: &[u32], piece_of: &[u32], origins: &[[f64; 2]]) -> VectorFeatures {
        let init = |label, contents: &[u8], copy: bool| {
            self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents,
                usage: wgpu::BufferUsages::STORAGE
                    | if copy { wgpu::BufferUsages::COPY_DST } else { wgpu::BufferUsages::empty() },
            })
        };
        let one = |v: &[u32]| if v.is_empty() { vec![0] } else { v.to_vec() };
        let (colors, piece_of) = (one(colors), one(piece_of));
        let mut o: Vec<[f32; 4]> = origins
            .iter()
            .map(|p| {
                let ([xh, xl], [yh, yl]) = (split(p[0]), split(p[1]));
                [xh, yh, xl, yl]
            })
            .collect();
        if o.is_empty() {
            o.push([0.0; 4]);
        }
        VectorFeatures {
            colors: init("vector colours", bytemuck::cast_slice(&colors), true),
            piece_of: init("vector pieces", bytemuck::cast_slice(&piece_of), false),
            origins: init("vector origins", bytemuck::cast_slice(&o), false),
            n: colors.len(),
            bytes: (colors.len() + piece_of.len()) * 4 + o.len() * 16,
        }
    }

    /// New colours for the same features (as many as when made).
    pub fn write_vector_colors(&self, c: &VectorFeatures, colors: &[u32]) {
        let n = colors.len().min(c.n);
        if n > 0 {
            self.queue.write_buffer(&c.colors, 0, bytemuck::cast_slice(&colors[..n]));
        }
    }

    /// A level of a layer: its points (with breaks between lines) and its fills' triangles (indices into them).
    pub fn upload_vector_geometry(
        &self,
        pts: &[VectorPoint],
        tris: &[u32],
        features: &VectorFeatures,
    ) -> VectorGeometry {
        let data: &[VectorPoint] = if pts.is_empty() { &[VectorPoint::BREAK] } else { pts };
        let points = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("vector points"),
            contents: bytemuck::cast_slice(data),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vector"),
            layout: &self.vector.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: points.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: features.colors.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: features.piece_of.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: features.origins.as_entire_binding() },
            ],
        });
        let index = (!tris.is_empty()).then(|| {
            let b = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("vector triangles"),
                contents: bytemuck::cast_slice(tris),
                usage: wgpu::BufferUsages::INDEX,
            });
            (b, tris.len() as u32)
        });
        VectorGeometry {
            group,
            n: pts.len() as u32,
            index,
            bytes: std::mem::size_of_val(data) + std::mem::size_of_val(tris),
        }
    }

    /// A level drawn, the parts of it in `ranges` (of its points for lines and points: a line from each point of
    /// a range to the next; of its triangles' indices for fills).
    pub fn draw_vector(
        &self,
        f: &mut MapFrame,
        g: &VectorGeometry,
        kind: VectorKind,
        ranges: Vec<Range<u32>>,
        view: &VectorView,
        style: &VectorStyle,
    ) {
        let index = match kind {
            VectorKind::Fill => match &g.index {
                Some((b, _)) => Some(b.clone()),
                None => return,
            },
            _ => None,
        };
        let limit = match kind {
            VectorKind::Fill => g.index.as_ref().map_or(0, |i| i.1),
            VectorKind::Lines => g.n.saturating_sub(1),
            VectorKind::Points { .. } => g.n,
        };
        let ranges: Vec<Range<u32>> = ranges
            .into_iter()
            .map(|r| {
                let end = if kind == VectorKind::Lines { r.end.saturating_sub(1) } else { r.end };
                r.start.min(limit)..end.min(limit)
            })
            .filter(|r| r.start < r.end)
            .collect();
        if ranges.is_empty() {
            return;
        }
        let ([cxh, cxl], [cyh, cyl]) = (split(view.centre[0]), split(view.centre[1]));
        let uniform = VectorUniform {
            a: [view.a[0] as f32, view.a[1] as f32],
            c_hi: [cxh, cyh],
            c_lo: [cxl, cyl],
            t0: [view.t0[0] as f32, view.t0[1] as f32],
            size: [f.size[0] as f32, f.size[1] as f32],
            width: style.width,
            halo: style.halo,
            halo_color: style.halo_color,
            feather: 0.6,
            alpha: style.alpha,
            round: u32::from(matches!(kind, VectorKind::Points { round: true })),
            _pad: 0,
        };
        let after = f.draws.len();
        f.vdraws.push(VectorDraw { uniform, group: g.group.clone(), kind, ranges, index, clip: f.clip, after });
    }

    /// Waits for the GPU to finish what was submitted (measurements).
    pub fn finish(&self) {
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
    }

    /// The uniforms of the frame's vector draws, at UNIFORM_STRIDE each (None: no vector draw).
    pub(crate) fn vector_uniforms(&self, f: &MapFrame) -> Option<wgpu::BindGroup> {
        if f.vdraws.is_empty() {
            return None;
        }
        let stride = crate::UNIFORM_STRIDE;
        let n = f.vdraws.len() as u64;
        let mut bytes = vec![0u8; (stride * n) as usize];
        for (i, d) in f.vdraws.iter().enumerate() {
            let at = i * stride as usize;
            bytes[at..at + size_of::<VectorUniform>()].copy_from_slice(bytemuck::bytes_of(&d.uniform));
        }
        let buffer = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("vector draws"),
            contents: &bytes,
            usage: wgpu::BufferUsages::UNIFORM,
        });
        Some(self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vector draws"),
            layout: &self.uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &buffer,
                    offset: 0,
                    size: wgpu::BufferSize::new(size_of::<VectorUniform>() as u64),
                }),
            }],
        }))
    }

    /// The frame's vector draw `i` (its uniforms in `group`), in the map's render pass; the raster pipeline is set
    /// again by the next raster draw.
    pub(crate) fn record_vector(
        &self,
        f: &MapFrame,
        i: usize,
        group: &wgpu::BindGroup,
        pass: &mut wgpu::RenderPass,
        size: [u32; 2],
    ) {
        let stride = crate::UNIFORM_STRIDE;
        let d = &f.vdraws[i];
        let [tw, th] = size;
        let [x, y, w, h] = d.clip.unwrap_or([0, 0, tw, th]);
        let (x, y) = (x.min(tw), y.min(th));
        let (w, h) = (w.min(tw - x), h.min(th - y));
        if w == 0 || h == 0 {
            return;
        }
        pass.set_scissor_rect(x, y, w, h);
        pass.set_bind_group(0, group, &[(i as u64 * stride) as u32]);
        pass.set_bind_group(1, &d.group, &[]);
        match d.kind {
            VectorKind::Lines => pass.set_pipeline(&self.vector.lines),
            VectorKind::Points { .. } => pass.set_pipeline(&self.vector.points),
            VectorKind::Fill => {
                pass.set_pipeline(&self.vector.fill);
                if let Some(b) = &d.index {
                    pass.set_index_buffer(b.slice(..), wgpu::IndexFormat::Uint32);
                }
            }
        }
        for r in &d.ranges {
            if d.kind == VectorKind::Fill {
                pass.draw_indexed(r.clone(), 0, 0..1);
            } else {
                pass.draw(0..6, r.clone());
            }
        }
    }
}
