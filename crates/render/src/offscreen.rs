//! Headless renderer: same pipeline, offscreen RGBA target with readback. Used by tests
//! and by headless benches (`render_us` without a window), including Windows DX12 zero-copy
//! GPU frame import.

use std::time::Instant;

use veloxdesk_core_traits::render::{RendererCaps, ScaleMode};
use veloxdesk_core_traits::video::Frame;
use veloxdesk_core_traits::{CoreError, Result};

use crate::pipeline::VideoPipeline;
use crate::zero_copy::{GpuImporter, create_importer, requested_features};

pub struct OffscreenRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: VideoPipeline,
    importer: Box<dyn GpuImporter>,
    target: wgpu::Texture,
    readback: wgpu::Buffer,
    size: (u32, u32),
    padded_row: u32,
    pub adapter_name: String,
    pub backend: wgpu::Backend,
}

impl OffscreenRenderer {
    /// Create a headless device (prefers DX12 on Windows for zero-copy import, with fallback
    /// to any available adapter).
    pub fn new(width: u32, height: u32) -> Result<Self> {
        #[cfg(windows)]
        if let Ok(r) = Self::new_with_backends(width, height, wgpu::Backends::DX12) {
            return Ok(r);
        }
        Self::new_with_backends(width, height, wgpu::Backends::all())
    }

    /// Create a headless device restricted to `backends`.
    pub fn new_with_backends(width: u32, height: u32, backends: wgpu::Backends) -> Result<Self> {
        let desc = wgpu::InstanceDescriptor {
            backends,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        };
        let instance = wgpu::Instance::new(desc);
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            apply_limit_buckets: false,
            compatible_surface: None,
        }))
        .map_err(|e| CoreError::unsupported(format!("no GPU adapter: {e}")))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("veloxdesk-offscreen"),
            required_features: requested_features(&adapter),
            required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
            ..Default::default()
        }))
        .map_err(|e| CoreError::platform(format!("request_device: {e}")))?;
        let importer = create_importer(&adapter, &device);
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
            importer,
            target,
            readback,
            size: (width, height),
            padded_row,
            adapter_name: info.name,
            backend: info.backend,
        })
    }

    /// Renderer capabilities (including `zero_copy_import: ["D3D11Texture"]` on DX12).
    pub fn caps(&self) -> RendererCaps {
        let backend = match self.backend {
            wgpu::Backend::Dx12 => "dx12",
            wgpu::Backend::Vulkan => "vulkan",
            wgpu::Backend::Metal => "metal",
            wgpu::Backend::Gl => "gl",
            wgpu::Backend::BrowserWebGpu => "webgpu",
            _ => "noop",
        };
        RendererCaps {
            backend,
            zero_copy_import: self.importer.supported_kinds(),
            hdr: false,
            present_modes: Vec::new(),
        }
    }

    /// Adapter LUID on Windows when zero-copy DX12 import is active.
    pub fn adapter_luid(&self) -> Option<u64> {
        self.importer.adapter_luid()
    }

    /// Upload/import + draw + wait for GPU completion. Returns `(elapsed_us, zero_copy)`.
    pub fn draw_frame(&mut self, frame: &Frame, scale: ScaleMode) -> Result<(u32, bool)> {
        let t0 = Instant::now();
        let zero_copy = self.pipeline.upload_or_import(frame, self.importer.as_mut())?;
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
        self.pipeline.release_previous_gpu_keepalive();
        Ok((t0.elapsed().as_micros() as u32, zero_copy))
    }

    /// Upload/import + draw + wait for GPU completion. Returns elapsed µs (CPU+GPU, no readback).
    pub fn draw(&mut self, frame: &Frame, scale: ScaleMode) -> Result<u32> {
        Ok(self.draw_frame(frame, scale)?.0)
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
