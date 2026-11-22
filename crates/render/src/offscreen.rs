//! Headless renderer: same pipeline, offscreen RGBA target with readback. Used by tests
//! and by headless benches (`render_us` without a window).

use std::time::Instant;

use veloxdesk_core_traits::render::ScaleMode;
use veloxdesk_core_traits::video::Frame;
use veloxdesk_core_traits::{CoreError, Result};

use crate::pipeline::VideoPipeline;

pub struct OffscreenRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: VideoPipeline,
    target: wgpu::Texture,
    readback: wgpu::Buffer,
    size: (u32, u32),
    padded_row: u32,
    pub adapter_name: String,
    pub backend: wgpu::Backend,
}

impl OffscreenRenderer {
    /// Create a headless device (any backend, falls back to software adapters).
    pub fn new(width: u32, height: u32) -> Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            apply_limit_buckets: false,
            compatible_surface: None,
        }))
        .map_err(|e| CoreError::unsupported(format!("no GPU adapter: {e}")))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("veloxdesk-offscreen"),
            required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
            ..Default::default()
        }))
        .map_err(|e| CoreError::platform(format!("request_device: {e}")))?;
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let pipeline = VideoPipeline::new(&device, &queue, format);
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("veloxdesk-offscreen-target"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let padded_row = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("veloxdesk-offscreen-readback"),
            size: (padded_row * height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let info = adapter.get_info();
        Ok(Self {
            device,
            queue,
            pipeline,
            target,
            readback,
            size: (width, height),
            padded_row,
            adapter_name: info.name,
            backend: info.backend,
        })
    }

    /// Upload + draw + wait for GPU completion. Returns elapsed µs (CPU+GPU, no readback).
    pub fn draw(&mut self, frame: &Frame, scale: ScaleMode) -> Result<u32> {
        let t0 = Instant::now();
        self.pipeline.upload(frame)?;
        self.pipeline.prepare(self.size, scale, None);
        let view = self.target.create_view(&Default::default());
        let mut enc = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("veloxdesk-offscreen"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.pipeline.draw(&mut pass);
        }
        self.queue.submit([enc.finish()]);
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| CoreError::platform(format!("poll: {e}")))?;
        Ok(t0.elapsed().as_micros() as u32)
    }

    /// Draw then read the target back as tightly packed RGBA8.
    pub fn render_rgba(&mut self, frame: &Frame, scale: ScaleMode) -> Result<Vec<u8>> {
        self.draw(frame, scale)?;
        let (w, h) = self.size;
        let mut enc = self.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.padded_row),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.queue.submit([enc.finish()]);
        let slice = self.readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| CoreError::platform(format!("poll: {e}")))?;
        rx.recv()
            .map_err(|_| CoreError::platform("map callback dropped"))?
            .map_err(|e| CoreError::platform(format!("map: {e}")))?;
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        {
            let data = slice.get_mapped_range().map_err(|e| CoreError::platform(format!("map range: {e}")))?;
            for row in data.chunks(self.padded_row as usize).take(h as usize) {
                out.extend_from_slice(&row[..(w * 4) as usize]);
            }
        }
        self.readback.unmap();
        Ok(out)
    }
}
