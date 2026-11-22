//! `VideoSurface`: window swapchain + video quad + egui (UI and HUD) in one render pass.

use std::sync::Arc;
use std::time::{Duration, Instant};

use veloxdesk_core_traits::metrics::HudSnapshot;
use veloxdesk_core_traits::render::{PresentMode, PresentStats, RendererCaps, ScaleMode, VideoRenderer};
use veloxdesk_core_traits::video::{CursorImage, Frame};
use veloxdesk_core_traits::{CoreError, Result};
use winit::window::Window;

use crate::hud;
use crate::layout::{VideoRect, window_to_video, window_to_video_clamped};
use crate::pipeline::VideoPipeline;
use crate::zero_copy::{GpuImporter, create_importer, requested_features};

/// Construction options.
#[derive(Debug, Clone)]
pub struct SurfaceOptions {
    pub present_mode: PresentMode,
    pub scale_mode: ScaleMode,
    /// Restrict backends (default: DX12 on Windows, Vulkan/Metal/GL elsewhere).
    pub backends: Option<wgpu::Backends>,
}

impl Default for SurfaceOptions {
    fn default() -> Self {
        Self { present_mode: PresentMode::Mailbox, scale_mode: ScaleMode::Fit, backends: None }
    }
}

/// Per-present timing breakdown (µs), for logs and benchmarks.
#[derive(Debug, Clone, Copy, Default)]
pub struct RenderBreakdown {
    /// CPU → GPU texture upload (`write_texture` calls).
    pub upload_us: u32,
    /// egui run + tessellation + texture/buffer updates.
    pub ui_us: u32,
    /// `get_current_texture` (blocks when the swapchain is full).
    pub acquire_us: u32,
    /// Command encoding + submit.
    pub submit_us: u32,
    /// `queue.present`.
    pub present_us: u32,
    pub total_us: u32,
}

fn backend_name(b: wgpu::Backend) -> &'static str {
    match b {
        wgpu::Backend::Dx12 => "dx12",
        wgpu::Backend::Vulkan => "vulkan",
        wgpu::Backend::Metal => "metal",
        wgpu::Backend::Gl => "gl",
        wgpu::Backend::BrowserWebGpu => "webgpu",
        _ => "noop",
    }
}

fn to_wgpu(mode: PresentMode) -> wgpu::PresentMode {
    match mode {
        PresentMode::Mailbox => wgpu::PresentMode::Mailbox,
        PresentMode::Immediate => wgpu::PresentMode::Immediate,
        PresentMode::Fifo => wgpu::PresentMode::Fifo,
    }
}

fn from_wgpu(mode: wgpu::PresentMode) -> Option<PresentMode> {
    match mode {
        wgpu::PresentMode::Mailbox => Some(PresentMode::Mailbox),
        wgpu::PresentMode::Immediate => Some(PresentMode::Immediate),
        wgpu::PresentMode::Fifo => Some(PresentMode::Fifo),
        _ => None,
    }
}

/// Pick the requested mode, else the lowest-latency supported one (Fifo always exists).
pub fn choose_present_mode(requested: PresentMode, supported: &[wgpu::PresentMode]) -> PresentMode {
    let order = match requested {
        PresentMode::Mailbox => [PresentMode::Mailbox, PresentMode::Immediate, PresentMode::Fifo],
        PresentMode::Immediate => [PresentMode::Immediate, PresentMode::Mailbox, PresentMode::Fifo],
        PresentMode::Fifo => [PresentMode::Fifo, PresentMode::Mailbox, PresentMode::Immediate],
    };
    order.into_iter().find(|m| supported.contains(&to_wgpu(*m))).unwrap_or(PresentMode::Fifo)
}

/// Window video presenter. Owns the wgpu device/queue/surface and the egui renderer.
pub struct VideoSurface {
    window: Arc<Window>,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    supported_modes: Vec<wgpu::PresentMode>,
    present_mode: PresentMode,
    scale_mode: ScaleMode,
    pipeline: VideoPipeline,
    importer: Box<dyn GpuImporter>,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
    cursor: Option<(i32, i32)>,
    hud: Option<HudSnapshot>,
    hud_extra: Option<String>,
    hud_visible: bool,
    video_rect: Option<VideoRect>,
    repaint_at: Option<Instant>,
    last: RenderBreakdown,
    needs_reconfigure: bool,
}

impl VideoSurface {
    /// Create the device and swapchain for `window`. Blocks on adapter/device creation.
    pub fn new(window: Arc<Window>, opts: SurfaceOptions) -> Result<Self> {
        let backends = opts.backends.unwrap_or(if cfg!(windows) {
            wgpu::Backends::DX12
        } else {
            wgpu::Backends::PRIMARY | wgpu::Backends::GL
        });
        let desc = wgpu::InstanceDescriptor {
            backends,
            ..wgpu::InstanceDescriptor::new_with_display_handle(Box::new(window.clone()))
        };
        let instance = wgpu::Instance::new(desc);
        let surface = instance
            .create_surface(window.clone())
            .map_err(|e| CoreError::platform(format!("create_surface: {e}")))?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            apply_limit_buckets: false,
            compatible_surface: Some(&surface),
        }))
        .map_err(|e| CoreError::unsupported(format!("no GPU adapter: {e}")))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("veloxdesk"),
            required_features: requested_features(&adapter),
            required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
            ..Default::default()
        }))
        .map_err(|e| CoreError::platform(format!("request_device: {e}")))?;
        let importer = create_importer(&adapter, &device);

        let caps = surface.get_capabilities(&adapter);
        // Non-sRGB target: video and egui are both gamma-encoded already.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| matches!(f, wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm))
            .or_else(|| caps.formats.iter().copied().find(|f| !f.is_srgb()))
            .unwrap_or(caps.formats[0]);
        let size = window.inner_size();
        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .ok_or_else(|| CoreError::unsupported("surface not supported by adapter"))?;
        let present_mode = choose_present_mode(opts.present_mode, &caps.present_modes);
        config.format = format;
        config.present_mode = to_wgpu(present_mode);
        config.desired_maximum_frame_latency = 1;
        config.alpha_mode = wgpu::CompositeAlphaMode::Auto;
        config.view_formats = vec![];
        surface.configure(&device, &config);

        let info = adapter.get_info();
        tracing::info!(
            adapter = %info.name,
            backend = backend_name(info.backend),
            ?format,
            present = ?present_mode,
            "video surface ready"
        );

        let pipeline = VideoPipeline::new(&device, &queue, format);
        let egui_ctx = egui::Context::default();
        let egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            window.theme(),
            Some(device.limits().max_texture_dimension_2d as usize),
        );
        let egui_renderer = egui_wgpu::Renderer::new(&device, format, egui_wgpu::RendererOptions::default());

        Ok(Self {
            window,
            adapter,
            device,
            queue,
            surface,
            config,
            supported_modes: caps.present_modes,
            present_mode,
            scale_mode: opts.scale_mode,
            pipeline,
            importer,
            egui_ctx,
            egui_state,
            egui_renderer,
            cursor: None,
            hud: None,
            hud_extra: None,
            hud_visible: false,
            video_rect: None,
            repaint_at: None,
            last: RenderBreakdown::default(),
            needs_reconfigure: false,
        })
    }

    pub fn window(&self) -> &Arc<Window> {
        &self.window
    }
    pub fn egui_ctx(&self) -> &egui::Context {
        &self.egui_ctx
    }
    pub fn adapter_info(&self) -> wgpu::AdapterInfo {
        self.adapter.get_info()
    }
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }
    /// Actually configured present mode (after fallback).
    pub fn present_mode(&self) -> PresentMode {
        self.present_mode
    }
    pub fn scale_mode(&self) -> ScaleMode {
        self.scale_mode
    }
    pub fn video_size(&self) -> Option<(u32, u32)> {
        self.pipeline.video_size()
    }
    pub fn last_breakdown(&self) -> RenderBreakdown {
        self.last
    }
    pub fn hud_visible(&self) -> bool {
        self.hud_visible
    }
    pub fn set_hud_visible(&mut self, on: bool) {
        self.hud_visible = on;
    }
    /// HUD data drawn by [`VideoRenderer::present`] / [`Self::render`] when visible.
    pub fn set_hud(&mut self, snapshot: HudSnapshot) {
        self.hud = Some(snapshot);
    }
    /// Extra status line rendered below the standard HUD (e.g. audio buffer, gamepads, pointer lock).
    pub fn set_hud_extra(&mut self, extra: Option<String>) {
        self.hud_extra = extra;
    }
    /// Adapter LUID on Windows when zero-copy DX12 import is active.
    pub fn adapter_luid(&self) -> Option<u64> {
        self.importer.adapter_luid()
    }
    /// When egui wants the next repaint (animations, tooltips); `None` = idle.
    pub fn repaint_deadline(&self) -> Option<Instant> {
        self.repaint_at
    }

    /// Feed a window event to egui. `consumed` = egui used it (don't forward to the host).
    pub fn on_window_event(&mut self, event: &winit::event::WindowEvent) -> egui_winit::EventResponse {
        self.egui_state.on_window_event(&self.window, event)
    }

    /// Map a window position (physical px) into video pixels (None = outside the picture).
    pub fn window_to_video(&self, pos: (f64, f64)) -> Option<(i32, i32)> {
        let rect = self.current_rect()?;
        window_to_video(&rect, self.pipeline.video_size()?, pos)
    }
    /// Clamped variant for drags leaving the picture.
    pub fn window_to_video_clamped(&self, pos: (f64, f64)) -> Option<(i32, i32)> {
        let rect = self.current_rect()?;
        Some(window_to_video_clamped(&rect, self.pipeline.video_size()?, pos))
    }

    fn current_rect(&self) -> Option<VideoRect> {
        let v = self.pipeline.video_size()?;
        Some(crate::layout::compute_video_rect(self.scale_mode, v, (self.config.width, self.config.height)))
    }

    fn reconfigure(&mut self) {
        self.surface.configure(&self.device, &self.config);
        self.needs_reconfigure = false;
    }

    /// Upload `frame` (if any), run `ui`, draw video + cursor + egui in one pass, present.
    pub fn render(&mut self, frame: Option<&Frame>, ui: &mut dyn FnMut(&mut egui::Ui)) -> Result<PresentStats> {
        let t0 = Instant::now();
        let mut bd = RenderBreakdown::default();
        let mut zero_copy = false;
        if let Some(f) = frame {
            zero_copy = self.pipeline.upload_or_import(f, self.importer.as_mut())?;
        }
        let t_up = Instant::now();
        bd.upload_us = (t_up - t0).as_micros() as u32;

        // ---- egui ----
        let raw = self.egui_state.take_egui_input(&self.window);
        let hud_visible = self.hud_visible;
        let hud_snap = self.hud.clone();
        let hud_extra = self.hud_extra.clone();
        let full = self.egui_ctx.run_ui(raw, |root| {
            ui(root);
            if hud_visible && let Some(s) = &hud_snap {
                hud::show_hud(root, s, hud_extra.as_deref());
            }
        });
        self.egui_state.handle_platform_output(&self.window, full.platform_output);
        self.repaint_at = full
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map(|v| v.repaint_delay)
            .filter(|d| *d < Duration::from_secs(3600))
            .map(|d| Instant::now() + d);
        let ppp = full.pixels_per_point;
        let jobs = self.egui_ctx.tessellate(full.shapes, ppp);
        for (id, deltas) in &full.textures_delta.set {
            for delta in deltas {
                self.egui_renderer.update_texture(&self.device, &self.queue, *id, delta);
            }
        }
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [self.config.width, self.config.height],
            pixels_per_point: ppp,
        };
        let mut encoder =
            self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("veloxdesk-frame") });
        let egui_cmds = self.egui_renderer.update_buffers(&self.device, &self.queue, &mut encoder, &jobs, &screen);
        let t_ui = Instant::now();
        bd.ui_us = (t_ui - t_up).as_micros() as u32;

        // ---- acquire ----
        if self.needs_reconfigure {
            self.reconfigure();
        }
        let st = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) => t,
            wgpu::CurrentSurfaceTexture::Suboptimal(t) => {
                self.needs_reconfigure = true;
                t
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                for id in &full.textures_delta.free {
                    self.egui_renderer.free_texture(id);
                }
                return Ok(PresentStats { render_us: t0.elapsed().as_micros() as u32, zero_copy: false });
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.reconfigure();
                match self.surface.get_current_texture() {
                    wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
                    other => return Err(CoreError::reinit(format!("surface unavailable: {other:?}"))),
                }
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err(CoreError::platform("surface acquire validation error"));
            }
        };
        let t_acq = Instant::now();
        bd.acquire_us = (t_acq - t_ui).as_micros() as u32;

        // ---- encode + submit ----
        let view = st.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let target = (self.config.width, self.config.height);
        self.video_rect = self.pipeline.prepare(target, self.scale_mode, self.cursor);
        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("veloxdesk-main"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                })
                .forget_lifetime();
            self.pipeline.draw(&mut pass);
            self.egui_renderer.render(&mut pass, &jobs, &screen);
        }
        self.queue.submit(egui_cmds.into_iter().chain(std::iter::once(encoder.finish())));
        let t_sub = Instant::now();
        bd.submit_us = (t_sub - t_acq).as_micros() as u32;

        self.window.pre_present_notify();
        self.queue.present(st);
        for id in &full.textures_delta.free {
            self.egui_renderer.free_texture(id);
        }
        let t_end = Instant::now();
        bd.present_us = (t_end - t_sub).as_micros() as u32;
        bd.total_us = (t_end - t0).as_micros() as u32;
        self.last = bd;
        Ok(PresentStats { render_us: bd.total_us, zero_copy })
    }
}

impl VideoRenderer for VideoSurface {
    fn caps(&self) -> RendererCaps {
        RendererCaps {
            backend: backend_name(self.adapter.get_info().backend),
            zero_copy_import: self.importer.supported_kinds(),
            hdr: false,
            present_modes: self.supported_modes.iter().copied().filter_map(from_wgpu).collect(),
        }
    }

    fn present(&mut self, frame: &Frame) -> Result<PresentStats> {
        self.render(Some(frame), &mut |_| {})
    }

    fn redraw(&mut self) -> Result<PresentStats> {
        self.render(None, &mut |_| {})
    }

    fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return; // minimized
        }
        if self.config.width != width || self.config.height != height {
            self.config.width = width;
            self.config.height = height;
            self.reconfigure();
        }
    }

    fn set_present_mode(&mut self, mode: PresentMode) -> Result<()> {
        let chosen = choose_present_mode(mode, &self.supported_modes);
        if chosen != mode {
            tracing::warn!(?mode, ?chosen, "present mode not supported, using fallback");
        }
        self.present_mode = chosen;
        self.config.present_mode = to_wgpu(chosen);
        self.reconfigure();
        Ok(())
    }

    fn set_scale_mode(&mut self, mode: ScaleMode) {
        self.scale_mode = mode;
    }

    fn set_cursor(&mut self, image: Option<&CursorImage>, x: i32, y: i32, visible: bool) {
        if image.is_some() {
            self.pipeline.set_cursor_image(image);
        }
        self.cursor = visible.then_some((x, y));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn present_mode_fallback() {
        use wgpu::PresentMode as W;
        assert_eq!(choose_present_mode(PresentMode::Mailbox, &[W::Fifo, W::Immediate]), PresentMode::Immediate);
        assert_eq!(choose_present_mode(PresentMode::Immediate, &[W::Fifo]), PresentMode::Fifo);
        assert_eq!(choose_present_mode(PresentMode::Fifo, &[W::Fifo, W::Mailbox]), PresentMode::Fifo);
        assert_eq!(choose_present_mode(PresentMode::Mailbox, &[W::Mailbox]), PresentMode::Mailbox);
    }
}
