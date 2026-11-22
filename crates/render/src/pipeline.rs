//! GPU video pipeline: CPU-frame upload (`queue.write_texture`) and the video/cursor quads.
//!
//! Independent of the window surface so it can also render into offscreen targets (tests,
//! headless benches). [`crate::VideoSurface`] wraps it together with egui.

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

use veloxdesk_core_traits::render::ScaleMode;
use veloxdesk_core_traits::video::{
    ColorRange, ColorSpace, CursorImage, Frame, FrameData, PixelFormat,
};
use veloxdesk_core_traits::{CoreError, Result};

use crate::color::{YuvMatrix, yuv_to_rgb_matrix_for};
use crate::layout::{VideoRect, compute_video_rect, video_to_window};
use crate::zero_copy::{GpuImporter, ImportedGpuViews};

const PARAMS_SIZE: u64 = 96;

/// Shader mode values (see `video.wgsl`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
enum Mode {
    RgbOpaque = 0,
    Nv12 = 1,
    I420 = 2,
    RgbaBlend = 3,
}

/// Where one texture's bytes live inside a frame's CPU planes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaneUpload {
    pub plane: usize,
    pub offset: usize,
    pub stride: usize,
    pub width: u32,
    pub height: u32,
    pub bytes_per_pixel: u32,
}

/// Compute texture uploads for a CPU frame (validating sizes). Supports the plane layouts
/// used across the codebase: one plane per component, or a single contiguous buffer.
pub fn plan_uploads(frame: &Frame, plane_lens: &[(usize, usize)]) -> Result<Vec<PlaneUpload>> {
    let (w, h) = (frame.width, frame.height);
    if w == 0 || h == 0 {
        return Err(CoreError::invalid("zero-sized frame"));
    }
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let get = |i: usize| -> Result<(usize, usize)> {
        plane_lens.get(i).copied().ok_or_else(|| CoreError::invalid("missing plane"))
    };
    let ups = match frame.format {
        PixelFormat::Bgra8 | PixelFormat::Rgba8 => {
            let (_, stride) = get(0)?;
            vec![PlaneUpload { plane: 0, offset: 0, stride, width: w, height: h, bytes_per_pixel: 4 }]
        }
        PixelFormat::Nv12 => {
            let (_, ys) = get(0)?;
            let y = PlaneUpload { plane: 0, offset: 0, stride: ys, width: w, height: h, bytes_per_pixel: 1 };
            let uv = if plane_lens.len() >= 2 {
                let (_, s) = get(1)?;
                PlaneUpload { plane: 1, offset: 0, stride: s, width: cw, height: ch, bytes_per_pixel: 2 }
            } else {
                PlaneUpload { plane: 0, offset: ys * h as usize, stride: ys, width: cw, height: ch, bytes_per_pixel: 2 }
            };
            vec![y, uv]
        }
        PixelFormat::P010 => {
            let (_, ys) = get(0)?;
            let y = PlaneUpload { plane: 0, offset: 0, stride: ys, width: w, height: h, bytes_per_pixel: 2 };
            let uv = if plane_lens.len() >= 2 {
                let (_, s) = get(1)?;
                PlaneUpload { plane: 1, offset: 0, stride: s, width: cw, height: ch, bytes_per_pixel: 4 }
            } else {
                PlaneUpload { plane: 0, offset: ys * h as usize, stride: ys, width: cw, height: ch, bytes_per_pixel: 4 }
            };
            vec![y, uv]
        }
        PixelFormat::I420 => {
            let (_, ys) = get(0)?;
            let y = PlaneUpload { plane: 0, offset: 0, stride: ys, width: w, height: h, bytes_per_pixel: 1 };
            if plane_lens.len() >= 3 {
                let (_, us) = get(1)?;
                let (_, vs) = get(2)?;
                vec![
                    y,
                    PlaneUpload { plane: 1, offset: 0, stride: us, width: cw, height: ch, bytes_per_pixel: 1 },
                    PlaneUpload { plane: 2, offset: 0, stride: vs, width: cw, height: ch, bytes_per_pixel: 1 },
                ]
            } else {
                let cs = ys.div_ceil(2);
                let u_off = ys * h as usize;
                let v_off = u_off + cs * ch as usize;
                vec![
                    y,
                    PlaneUpload { plane: 0, offset: u_off, stride: cs, width: cw, height: ch, bytes_per_pixel: 1 },
                    PlaneUpload { plane: 0, offset: v_off, stride: cs, width: cw, height: ch, bytes_per_pixel: 1 },
                ]
            }
        }
        PixelFormat::Yuv444 => {
            let (_, ys) = get(0)?;
            let y = PlaneUpload { plane: 0, offset: 0, stride: ys, width: w, height: h, bytes_per_pixel: 1 };
            if plane_lens.len() >= 3 {
                let (_, us) = get(1)?;
                let (_, vs) = get(2)?;
                vec![
                    y,
                    PlaneUpload { plane: 1, offset: 0, stride: us, width: w, height: h, bytes_per_pixel: 1 },
                    PlaneUpload { plane: 2, offset: 0, stride: vs, width: w, height: h, bytes_per_pixel: 1 },
                ]
            } else {
                let u_off = ys * h as usize;
                let v_off = u_off + ys * h as usize;
                vec![
                    y,
                    PlaneUpload { plane: 0, offset: u_off, stride: ys, width: w, height: h, bytes_per_pixel: 1 },
                    PlaneUpload { plane: 0, offset: v_off, stride: ys, width: w, height: h, bytes_per_pixel: 1 },
                ]
            }
        }
        other => return Err(CoreError::unsupported(format!("renderer: pixel format {other:?} not supported yet"))),
    };
    for u in &ups {
        let (len, _) = get(u.plane)?;
        let row = (u.width * u.bytes_per_pixel) as usize;
        if u.stride < row {
            return Err(CoreError::invalid("plane stride smaller than row"));
        }
        let need = u.offset + u.stride * (u.height as usize - 1) + row;
        if len < need {
            return Err(CoreError::invalid(format!("plane {} too short: {len} < {need}", u.plane)));
        }
    }
    Ok(ups)
}

struct VideoTextures {
    format: PixelFormat,
    width: u32,
    height: u32,
    /// Actual sampling format (e.g. `Nv12` when `P010` is downsampled to 8-bit on adapters
    /// without `TEXTURE_FORMAT_16BIT_NORM`).
    sample_format: PixelFormat,
    textures: Vec<wgpu::Texture>,
    bind_group: wgpu::BindGroup,
}

struct ActiveGpuFrame {
    format: PixelFormat,
    width: u32,
    height: u32,
    tex_size: (u32, u32),
    cache_key: u64,
    keepalive: Option<Arc<dyn Any + Send + Sync>>,
}

struct CursorTexture {
    id: u64,
    size: (u32, u32),
    hotspot: (u32, u32),
    bind_group: wgpu::BindGroup,
}

/// Video + cursor quad renderer for one target format.
pub struct VideoPipeline {
    device: wgpu::Device,
    queue: wgpu::Queue,
    layout: wgpu::BindGroupLayout,
    opaque: wgpu::RenderPipeline,
    blend: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    dummy: wgpu::TextureView,
    supports_16bit_norm: bool,
    video: Option<VideoTextures>,
    gpu_bind_groups: HashMap<u64, wgpu::BindGroup>,
    active_gpu: Option<ActiveGpuFrame>,
    /// Previous GPU frame keepalive kept alive across one submit so the GPU finishes sampling
    /// before the decoder's `TexturePool` can recycle the backing D3D11 texture.
    prev_gpu_keepalive: Option<Arc<dyn Any + Send + Sync>>,
    video_params: wgpu::Buffer,
    color: (ColorSpace, ColorRange),
    cursor: Option<CursorTexture>,
    cursor_params: wgpu::Buffer,
    cursor_visible: bool,
}

fn params_bytes(
    rect: [f32; 4],
    uv: [f32; 4],
    m: &YuvMatrix,
    mode: Mode,
    hdr_pq: bool,
) -> [u8; PARAMS_SIZE as usize] {
    let mut out = [0u8; PARAMS_SIZE as usize];
    let floats = rect.iter().chain(uv.iter()).chain(m.iter().flatten());
    for (i, f) in floats.enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&f.to_le_bytes());
    }
    out[80..84].copy_from_slice(&(mode as u32).to_le_bytes());
    out[84..88].copy_from_slice(&(u32::from(hdr_pq)).to_le_bytes());
    out
}

impl VideoPipeline {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("veloxdesk-video"),
            source: wgpu::ShaderSource::Wgsl(include_str!("video.wgsl").into()),
        });
        let tex_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("veloxdesk-video-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(PARAMS_SIZE),
                    },
                    count: None,
                },
                tex_entry(1),
                tex_entry(2),
                tex_entry(3),
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("veloxdesk-video-pl"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let make = |blend: Option<wgpu::BlendState>, label| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pl),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: target_format,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let opaque = make(None, "veloxdesk-video-opaque");
        let blend = make(Some(wgpu::BlendState::ALPHA_BLENDING), "veloxdesk-cursor-blend");
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("veloxdesk-video-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let dummy_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("veloxdesk-dummy"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let dummy = dummy_tex.create_view(&Default::default());
        let mk_buf = |label| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: PARAMS_SIZE,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        Self {
            device: device.clone(),
            queue: queue.clone(),
            layout,
            opaque,
            blend,
            sampler,
            dummy,
            supports_16bit_norm: device.features().contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM),
            video: None,
            gpu_bind_groups: HashMap::new(),
            active_gpu: None,
            prev_gpu_keepalive: None,
            video_params: mk_buf("veloxdesk-video-params"),
            color: (ColorSpace::Bt709, ColorRange::Limited),
            cursor: None,
            cursor_params: mk_buf("veloxdesk-cursor-params"),
            cursor_visible: false,
        }
    }

    /// Size of the currently uploaded/imported video, if any.
    pub fn video_size(&self) -> Option<(u32, u32)> {
        self.active_gpu
            .as_ref()
            .map(|g| (g.width, g.height))
            .or_else(|| self.video.as_ref().map(|v| (v.width, v.height)))
    }

    fn bind_group(&self, label: &str, buf: &wgpu::Buffer, views: [&wgpu::TextureView; 3]) -> wgpu::BindGroup {
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(views[0]) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(views[1]) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(views[2]) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        })
    }

    fn create_texture(&self, label: &str, w: u32, h: u32, format: wgpu::TextureFormat) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
    }

    fn ensure_video_textures(&mut self, format: PixelFormat, w: u32, h: u32) {
        if let Some(v) = &self.video
            && v.format == format
            && v.width == w
            && v.height == h
        {
            return;
        }
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        use wgpu::TextureFormat as T;
        let (sample_format, specs): (PixelFormat, Vec<(u32, u32, T)>) = match format {
            PixelFormat::Bgra8 => (format, vec![(w, h, T::Bgra8Unorm)]),
            PixelFormat::Rgba8 => (format, vec![(w, h, T::Rgba8Unorm)]),
            PixelFormat::Nv12 => (format, vec![(w, h, T::R8Unorm), (cw, ch, T::Rg8Unorm)]),
            PixelFormat::P010 if self.supports_16bit_norm => {
                (PixelFormat::P010, vec![(w, h, T::R16Unorm), (cw, ch, T::Rg16Unorm)])
            }
            PixelFormat::P010 => (PixelFormat::Nv12, vec![(w, h, T::R8Unorm), (cw, ch, T::Rg8Unorm)]),
            PixelFormat::Yuv444 => {
                (format, vec![(w, h, T::R8Unorm), (w, h, T::R8Unorm), (w, h, T::R8Unorm)])
            }
            _ => (format, vec![(w, h, T::R8Unorm), (cw, ch, T::R8Unorm), (cw, ch, T::R8Unorm)]),
        };
        let textures: Vec<wgpu::Texture> =
            specs.iter().map(|&(tw, th, f)| self.create_texture("veloxdesk-video-plane", tw, th, f)).collect();
        let views: Vec<wgpu::TextureView> = textures.iter().map(|t| t.create_view(&Default::default())).collect();
        let v = |i: usize| views.get(i).unwrap_or(&self.dummy);
        let bind_group = self.bind_group("veloxdesk-video-bg", &self.video_params, [v(0), v(1), v(2)]);
        tracing::debug!(?format, ?sample_format, w, h, "video textures (re)created");
        self.video = Some(VideoTextures { format, width: w, height: h, sample_format, textures, bind_group });
    }

    /// Upload a CPU frame or zero-copy import a GPU frame via `importer`. Returns `true` when
    /// the zero-copy GPU path was used.
    pub fn upload_or_import(&mut self, frame: &Frame, importer: &mut dyn GpuImporter) -> Result<bool> {
        match &frame.data {
            FrameData::Gpu(g) => {
                let imported = importer.import(g, frame.format, frame.width, frame.height)?;
                self.bind_imported_gpu(frame, imported, g.keepalive.clone());
                Ok(true)
            }
            FrameData::Cpu(_) => {
                self.upload(frame)?;
                Ok(false)
            }
        }
    }

    fn bind_imported_gpu(
        &mut self,
        frame: &Frame,
        imported: ImportedGpuViews,
        keepalive: Option<Arc<dyn Any + Send + Sync>>,
    ) {
        let key = imported.cache_key;
        if !self.gpu_bind_groups.contains_key(&key) {
            if self.gpu_bind_groups.len() >= 16 {
                self.gpu_bind_groups.clear();
            }
            let bg = self.bind_group(
                "veloxdesk-gpu-video-bg",
                &self.video_params,
                [&imported.y_view, &imported.uv_view, &self.dummy],
            );
            self.gpu_bind_groups.insert(key, bg);
        }
        self.color = (frame.color_space, frame.color_range);
        self.prev_gpu_keepalive = self.active_gpu.take().and_then(|g| g.keepalive);
        self.active_gpu = Some(ActiveGpuFrame {
            format: frame.format,
            width: frame.width,
            height: frame.height,
            tex_size: imported.tex_size,
            cache_key: key,
            keepalive,
        });
    }

    /// Release the previous GPU frame keepalive after GPU submission/wait.
    pub fn release_previous_gpu_keepalive(&mut self) {
        self.prev_gpu_keepalive = None;
    }

    /// Upload a frame (CPU path). GPU frames return `Unsupported` (use [`Self::upload_or_import`]).
    pub fn upload(&mut self, frame: &Frame) -> Result<()> {
        let planes = match &frame.data {
            FrameData::Cpu(p) => p,
            FrameData::Gpu(_) => {
                return Err(CoreError::unsupported("renderer: GPU frame requires upload_or_import"));
            }
        };
        self.prev_gpu_keepalive = self.active_gpu.take().and_then(|g| g.keepalive);
        let lens: Vec<(usize, usize)> = planes.iter().map(|p| (p.data.len(), p.stride)).collect();
        let ups = plan_uploads(frame, &lens)?;
        self.ensure_video_textures(frame.format, frame.width, frame.height);
        self.color = (frame.color_space, frame.color_range);
        let video = self.video.as_ref().expect("textures ensured");
        if frame.format == PixelFormat::P010 && video.sample_format == PixelFormat::Nv12 {
            // Fallback for adapters without TEXTURE_FORMAT_16BIT_NORM: take MSB of each u16.
            for (u, tex) in ups.iter().zip(video.textures.iter()) {
                let src = &planes[u.plane].data;
                let dst_bpp = (u.bytes_per_pixel / 2) as usize;
                let dst_row = u.width as usize * dst_bpp;
                let mut down = vec![0u8; dst_row * u.height as usize];
                for row in 0..u.height as usize {
                    let s_row = &src[u.offset + row * u.stride..u.offset + row * u.stride + u.width as usize * u.bytes_per_pixel as usize];
                    let d_row = &mut down[row * dst_row..(row + 1) * dst_row];
                    for (i, d) in d_row.iter_mut().enumerate() {
                        *d = s_row[i * 2 + 1];
                    }
                }
                self.queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: tex,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    &down,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(dst_row as u32),
                        rows_per_image: Some(u.height),
                    },
                    wgpu::Extent3d { width: u.width, height: u.height, depth_or_array_layers: 1 },
                );
            }
            return Ok(());
        }
        for (u, tex) in ups.iter().zip(video.textures.iter()) {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &planes[u.plane].data,
                wgpu::TexelCopyBufferLayout {
                    offset: u.offset as u64,
                    bytes_per_row: Some(u.stride as u32),
                    rows_per_image: Some(u.height),
                },
                wgpu::Extent3d { width: u.width, height: u.height, depth_or_array_layers: 1 },
            );
        }
        Ok(())
    }

    /// Set (or clear) the cursor bitmap. Re-uploads only when the id changes.
    pub fn set_cursor_image(&mut self, image: Option<&CursorImage>) {
        let Some(img) = image else {
            self.cursor = None;
            return;
        };
        if self.cursor.as_ref().is_some_and(|c| c.id == img.id) {
            return;
        }
        let need = (img.width * img.height * 4) as usize;
        if img.width == 0 || img.height == 0 || img.rgba.len() < need {
            tracing::warn!(id = img.id, "invalid cursor image ignored");
            return;
        }
        let tex = self.create_texture("veloxdesk-cursor", img.width, img.height, wgpu::TextureFormat::Rgba8Unorm);
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &img.rgba[..need],
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(img.width * 4), rows_per_image: None },
            wgpu::Extent3d { width: img.width, height: img.height, depth_or_array_layers: 1 },
        );
        let view = tex.create_view(&Default::default());
        let bind_group = self.bind_group("veloxdesk-cursor-bg", &self.cursor_params, [&view, &self.dummy, &self.dummy]);
        self.cursor = Some(CursorTexture {
            id: img.id,
            size: (img.width, img.height),
            hotspot: (img.hotspot_x, img.hotspot_y),
            bind_group,
        });
    }

    /// Write per-frame uniforms. `cursor` = position in video pixels (None = hidden).
    /// Returns the video rectangle in target pixels.
    pub fn prepare(&mut self, target: (u32, u32), scale: ScaleMode, cursor: Option<(i32, i32)>) -> Option<VideoRect> {
        let (vsize, format, sample_format, uv_rect) = if let Some(g) = &self.active_gpu {
            let u1 = (g.width as f32 / g.tex_size.0.max(1) as f32).min(1.0);
            let v1 = (g.height as f32 / g.tex_size.1.max(1) as f32).min(1.0);
            ((g.width, g.height), g.format, g.format, [0.0, 0.0, u1, v1])
        } else if let Some(v) = &self.video {
            ((v.width, v.height), v.format, v.sample_format, [0.0, 0.0, 1.0, 1.0])
        } else {
            return None;
        };
        let rect = compute_video_rect(scale, vsize, target);
        let mode = match format {
            PixelFormat::Nv12 | PixelFormat::P010 => Mode::Nv12,
            PixelFormat::I420 | PixelFormat::Yuv444 => Mode::I420,
            _ => Mode::RgbOpaque,
        };
        let m = yuv_to_rgb_matrix_for(sample_format, self.color.0, self.color.1);
        let hdr_pq = self.color.0 == ColorSpace::Bt2020Pq;
        self.queue
            .write_buffer(&self.video_params, 0, &params_bytes(rect.to_ndc(target), uv_rect, &m, mode, hdr_pq));

        self.cursor_visible = false;
        if let (Some(c), Some((cx, cy))) = (&self.cursor, cursor) {
            // Cursor scales with the video so it matches the remote desktop's size.
            let sx = rect.w / vsize.0.max(1) as f32;
            let sy = rect.h / vsize.1.max(1) as f32;
            let (wx, wy) = video_to_window(&rect, vsize, (cx as f32 - c.hotspot.0 as f32, cy as f32 - c.hotspot.1 as f32));
            let crect = VideoRect { x: wx, y: wy, w: c.size.0 as f32 * sx, h: c.size.1 as f32 * sy };
            self.queue.write_buffer(
                &self.cursor_params,
                0,
                &params_bytes(crect.to_ndc(target), [0.0, 0.0, 1.0, 1.0], &m, Mode::RgbaBlend, false),
            );
            self.cursor_visible = true;
        }
        Some(rect)
    }

    /// Record the draw calls (video first, then cursor) into an open render pass.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        let bg = if let Some(g) = &self.active_gpu {
            self.gpu_bind_groups.get(&g.cache_key)
        } else {
            self.video.as_ref().map(|v| &v.bind_group)
        };
        let Some(bg) = bg else { return };
        pass.set_pipeline(&self.opaque);
        pass.set_bind_group(0, bg, &[]);
        pass.draw(0..4, 0..1);
        if self.cursor_visible
            && let Some(c) = &self.cursor
        {
            pass.set_pipeline(&self.blend);
            pass.set_bind_group(0, &c.bind_group, &[]);
            pass.draw(0..4, 0..1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pattern::nv12_test_frame;

    fn lens(f: &Frame) -> Vec<(usize, usize)> {
        match &f.data {
            FrameData::Cpu(p) => p.iter().map(|p| (p.data.len(), p.stride)).collect(),
            FrameData::Gpu(_) => vec![],
        }
    }

    #[test]
    fn nv12_two_planes() {
        let f = nv12_test_frame(1919, 1079, 0);
        let ups = plan_uploads(&f, &lens(&f)).unwrap();
        assert_eq!(ups.len(), 2);
        assert_eq!((ups[1].width, ups[1].height, ups[1].bytes_per_pixel), (960, 540, 2));
    }

    #[test]
    fn nv12_single_plane_and_short_buffer() {
        let mut f = nv12_test_frame(64, 32, 0);
        f.data = FrameData::Cpu(vec![]);
        let ups = plan_uploads(&f, &[(64 * 32 * 3 / 2, 64)]).unwrap();
        assert_eq!(ups[1].offset, 64 * 32);
        assert!(plan_uploads(&f, &[(64 * 32, 64)]).is_err());
    }

    #[test]
    fn i420_single_plane() {
        let mut f = nv12_test_frame(64, 32, 0);
        f.format = PixelFormat::I420;
        let ups = plan_uploads(&f, &[(64 * 32 * 3 / 2, 64)]).unwrap();
        assert_eq!(ups.len(), 3);
        assert_eq!(ups[2].offset, 64 * 32 + 32 * 16);
    }

    #[test]
    fn params_layout() {
        let m = [[1.0; 4]; 3];
        let b = params_bytes([1.0, 2.0, 3.0, 4.0], [0.0; 4], &m, Mode::I420, true);
        assert_eq!(f32::from_le_bytes(b[4..8].try_into().unwrap()), 2.0);
        assert_eq!(u32::from_le_bytes(b[80..84].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(b[84..88].try_into().unwrap()), 1);
    }
}
