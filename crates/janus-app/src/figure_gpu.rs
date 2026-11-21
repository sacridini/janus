//! A figure drawn by the GPU: the shapes of a figure as shown (figure.rs) tessellated and painted by egui_wgpu's renderer,
//! the one the interface is drawn with, into a texture of their own, and read back as RGBA8. Only the target differs
//! from the window's (a texture instead of the swap chain), so the pixels are what the window shows, at as many
//! pixels per point as asked.

use eframe::egui_wgpu::{self, RenderState, RendererOptions, ScreenDescriptor};

use crate::figure::Rendered;

impl Rendered {
    /// The figure as RGBA8 rows from the top, not premultiplied, drawn by the GPU of `rs`: (pixels, width, height).
    /// Waits for the GPU (a few milliseconds), so it is for a worker thread or a figure that is small.
    pub fn to_rgba_gpu(&self, rs: &RenderState) -> Result<(Vec<u8>, usize, usize), String> {
        let [w, h_full] =
            [(self.size.x * self.ppp).round().max(1.0) as u32, (self.size.y * self.ppp).round().max(1.0) as u32];
        let top = ((self.crop * self.ppp).round() as u32).min(h_full.saturating_sub(1));
        let h = h_full - top;
        let max = rs.device.limits().max_texture_dimension_2d;
        if w > max || h_full > max {
            return Err(format!(
                "{w} x {h_full} px is more than this GPU draws ({max} px a side): use a smaller scale"
            ));
        }
        let device = &rs.device;
        let queue = &rs.queue;
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let mut renderer = egui_wgpu::Renderer::new(device, format, RendererOptions::default());
        for (id, delta) in &self.deltas {
            renderer.update_texture(device, queue, *id, delta);
        }
        let prims = self.ctx.tessellate(self.shapes.clone(), self.ppp);
        let screen = ScreenDescriptor { size_in_pixels: [w, h_full], pixels_per_point: self.ppp };
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("figure"),
            size: wgpu::Extent3d { width: w, height: h_full, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&Default::default());
        let clear = if self.transparent {
            wgpu::Color::TRANSPARENT
        } else {
            let [r, g, b, _] = self.background.to_array();
            wgpu::Color { r: f64::from(r) / 255.0, g: f64::from(g) / 255.0, b: f64::from(b) / 255.0, a: 1.0 }
        };
        let row = (w * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("figure readback"),
            size: u64::from(row) * u64::from(h_full),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("figure") });
        let extra = renderer.update_buffers(device, queue, &mut enc, &prims, &screen);
        {
            let mut pass = enc
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("figure"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations { load: wgpu::LoadOp::Clear(clear), store: wgpu::StoreOp::Store },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                })
                .forget_lifetime();
            renderer.render(&mut pass, &prims, &screen);
        }
        enc.copy_texture_to_buffer(
            target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: None },
            },
            wgpu::Extent3d { width: w, height: h_full, depth_or_array_layers: 1 },
        );
        queue.submit(extra.into_iter().chain([enc.finish()]));
        let (tx, rx) = std::sync::mpsc::channel();
        buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| format!("the GPU did not finish the figure: {e}"))?;
        rx.recv().map_err(|e| format!("the figure's pixels did not come back: {e}"))?.map_err(|e| e.to_string())?;
        let data = buffer.slice(..).get_mapped_range().map_err(|e| e.to_string())?;
        let mut out = Vec::with_capacity(w as usize * h as usize * 4);
        for y in top..h_full {
            let at = y as usize * row as usize;
            out.extend_from_slice(&data[at..at + w as usize * 4]);
        }
        drop(data);
        buffer.unmap();
        if self.transparent {
            // the renderer's output is premultiplied
            for p in out.as_chunks_mut::<4>().0 {
                let a = u32::from(p[3]);
                if a != 0 && a != 255 {
                    for c in &mut p[..3] {
                        *c = ((u32::from(*c) * 255 + a / 2) / a).min(255) as u8;
                    }
                }
            }
        }
        Ok((out, w as usize, h as usize))
    }

    /// The figure as PNG bytes (the pixels from the GPU).
    pub fn to_png_gpu(&self, rs: &RenderState) -> Result<Vec<u8>, String> {
        let (rgba, w, h) = self.to_rgba_gpu(rs)?;
        crate::figure::png_bytes(&rgba, w, h, self.dpi)
    }

    /// The figure as an image for the clipboard (the pixels from the GPU).
    pub fn to_color_image_gpu(&self, rs: &RenderState) -> Result<egui::ColorImage, String> {
        let (rgba, w, h) = self.to_rgba_gpu(rs)?;
        Ok(egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba))
    }
}
