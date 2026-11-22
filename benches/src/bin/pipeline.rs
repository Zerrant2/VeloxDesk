//! Loopback pipeline benchmark.
//!
//! capture (or `--synthetic` frames) → `create_encoder` → `VideoSender` → loopback QUIC
//! (optionally through the impairment proxy) → `VideoReceiver` → `create_decoder`,
//! reporting p50/p95/p99 per stage and end-to-end vs. the ARCHITECTURE §1 budget.
//!
//! Host and viewer run in the same process, so they share one monotonic clock and
//! glass-to-glass (capture → decoded) is measured exactly, without clock sync.
//!
//! ```text
//! cargo run -p veloxdesk-benches --release --bin pipeline -- --synthetic --frames 600 --loss 0.03
//! ```

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use bytes::Bytes;
use clap::Parser;
use crossbeam_channel::{Receiver, TrySendError, bounded};

use veloxdesk_benches::impair::{BandwidthCap, Direction, ImpairmentConfig, LossModel, UdpImpairProxy};
use veloxdesk_benches::stats::{Stage, latency_table};
use veloxdesk_core_traits::Timestamp;
use veloxdesk_core_traits::transport::TransportChannel;
use veloxdesk_core_traits::video::{
    CaptureConfig, ChromaFormat, ColorRange, ColorSpace, DecoderConfig, EncodeOptions, EncodedPacket, EncoderConfig,
    FrameCapture, Frame, FrameData, PixelFormat, Plane, RateControl, VideoCodec,
};
use veloxdesk_crypto::DeviceIdentity;
use veloxdesk_transport::{VideoReceiver, VideoSender};

#[derive(Parser, Debug, Clone)]
#[command(about = "VeloxDesk loopback pipeline latency benchmark")]
struct Args {
    /// Use synthetic BGRA frames instead of screen capture.
    #[arg(long)]
    synthetic: bool,
    /// Number of frames to send.
    #[arg(long, default_value_t = 600)]
    frames: u32,
    #[arg(long, default_value_t = 1920)]
    width: u32,
    #[arg(long, default_value_t = 1080)]
    height: u32,
    #[arg(long, default_value_t = 60)]
    fps: u32,
    #[arg(long, default_value_t = 20_000)]
    bitrate_kbps: u32,
    /// h264 | h265 | av1
    #[arg(long, default_value = "h264")]
    codec: String,
    /// Prefer hardware encoder/decoder (falls back to SW).
    #[arg(long)]
    hw: bool,
    /// Prefer GPU capture frames (zero-copy into the encoder when supported).
    #[arg(long)]
    gpu: bool,
    /// Enable GPU zero-copy decode -> DX12 wgpu import (implies --hw --render; on Windows
    /// with --synthetic also uses a GPU-resident D3D11 texture ring on the host).
    #[arg(long)]
    zero_copy: bool,
    /// Include headless wgpu render stage (`OffscreenRenderer::draw_frame`).
    #[arg(long)]
    render: bool,
    /// FEC parity percent (0 = sender default).
    #[arg(long, default_value_t = 20)]
    parity: u8,
    /// Random datagram loss (0..1) host→viewer. Enables the impairment proxy.
    #[arg(long, default_value_t = 0.0)]
    loss: f64,
    /// Mean burst length for Gilbert–Elliott loss (with --loss as the average rate).
    #[arg(long)]
    burst: Option<f64>,
    /// One-way latency added by the proxy (ms, both directions).
    #[arg(long, default_value_t = 0.0)]
    latency_ms: f64,
    #[arg(long, default_value_t = 0.0)]
    jitter_ms: f64,
    /// Bandwidth cap host→viewer (kbps).
    #[arg(long)]
    bw_kbps: Option<u32>,
    /// Write the markdown report here as well as stdout.
    #[arg(long)]
    report: Option<std::path::PathBuf>,
}

impl Args {
    fn codec(&self) -> Result<VideoCodec> {
        Ok(match self.codec.to_ascii_lowercase().as_str() {
            "h264" | "avc" => VideoCodec::H264,
            "h265" | "hevc" => VideoCodec::H265,
            "av1" => VideoCodec::Av1,
            other => return Err(anyhow!("unknown codec {other}")),
        })
    }
    fn impaired(&self) -> bool {
        self.loss > 0.0 || self.latency_ms > 0.0 || self.jitter_ms > 0.0 || self.bw_kbps.is_some()
    }
    fn down_cfg(&self) -> ImpairmentConfig {
        let loss = match (self.loss, self.burst) {
            (l, _) if l <= 0.0 => LossModel::None,
            (l, Some(b)) => LossModel::bursty(l, b),
            (l, None) => LossModel::Random(l),
        };
        let mut c = ImpairmentConfig::default().with_loss(loss).with_latency(ms(self.latency_ms), ms(self.jitter_ms));
        if let Some(bw) = self.bw_kbps {
            c = c.with_bandwidth(BandwidthCap::new(bw));
        }
        c
    }
    fn up_cfg(&self) -> ImpairmentConfig {
        ImpairmentConfig::default().with_latency(ms(self.latency_ms), ms(self.jitter_ms))
    }
}

fn ms(v: f64) -> Duration {
    Duration::from_secs_f64(v.max(0.0) / 1000.0)
}

/// Host-side per-frame record, joined with the viewer side by `frame_id`.
#[derive(Debug, Clone, Copy, Default)]
struct HostRec {
    capture_ts: u64,
    capture_us: u32,
    convert_us: u32,
    encode_us: u32,
    packetize_us: u32,
    /// Timestamp when `send_frame` returned (all datagrams queued).
    sent_ts: u64,
    bytes: u32,
    keyframe: bool,
}

/// Viewer-side per-frame record.
#[derive(Debug)]
struct ViewerFrame {
    packet: EncodedPacket,
    recv_complete: u64,
    recovered: u32,
}

#[derive(Default)]
struct Shared {
    host: Mutex<HashMap<u32, HostRec>>,
    frames_sent: AtomicU64,
    frames_complete: AtomicU64,
    shards_recovered: AtomicU64,
    feedback_msgs: AtomicU64,
    dropped_latest_wins: AtomicU64,
    pli_requested: AtomicBool,
    done: AtomicBool,
}

// ───────────────────────────── sources ─────────────────────────────

trait Source {
    fn name(&self) -> String;
    /// Returns the frame and `(capture_us, convert_us)`.
    fn next(&mut self) -> Result<(Frame, u32, u32)>;
}

/// Synthetic desktop: static gradient + a moving block and a scrolling band, copied
/// into a fresh `Bytes` each frame (cost ≈ a CPU readback).
struct Synthetic {
    w: u32,
    h: u32,
    buf: Vec<u8>,
    n: u32,
}

impl Synthetic {
    fn new(w: u32, h: u32) -> Self {
        let mut buf = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                buf[i] = (x * 255 / w) as u8;
                buf[i + 1] = (y * 255 / h) as u8;
                buf[i + 2] = ((x ^ y) & 0xFF) as u8;
                buf[i + 3] = 255;
            }
        }
        Self { w, h, buf, n: 0 }
    }
}

impl Source for Synthetic {
    fn name(&self) -> String {
        format!("synthetic {}x{}", self.w, self.h)
    }
    fn next(&mut self) -> Result<(Frame, u32, u32)> {
        let t = Instant::now();
        let (w, h) = (self.w as usize, self.h as usize);
        let n = self.n as usize;
        self.n += 1;
        // Moving 256x256 block.
        let bx = (n * 7) % w.saturating_sub(256).max(1);
        let by = (n * 3) % h.saturating_sub(256).max(1);
        for y in by..(by + 256).min(h) {
            let row = &mut self.buf[(y * w + bx) * 4..(y * w + (bx + 256).min(w)) * 4];
            for (k, px) in row.chunks_exact_mut(4).enumerate() {
                px[0] = (k as u8).wrapping_add(n as u8);
                px[1] = (y as u8).wrapping_mul(3);
                px[2] = (n as u8).wrapping_mul(5);
            }
        }
        // Scrolling text-like band (high-frequency detail).
        let band_y = (n * 4) % h.saturating_sub(32).max(1);
        for y in band_y..(band_y + 32).min(h) {
            let row = &mut self.buf[y * w * 4..(y + 1) * w * 4];
            for (x, px) in row.chunks_exact_mut(4).enumerate() {
                let v = if ((x + n) / 3 + y) % 5 == 0 { 0 } else { 230 };
                px[0] = v;
                px[1] = v;
                px[2] = v;
            }
        }
        let data = Bytes::copy_from_slice(&self.buf);
        let frame = Frame {
            width: self.w,
            height: self.h,
            format: PixelFormat::Bgra8,
            color_space: ColorSpace::Srgb,
            color_range: ColorRange::Full,
            data: FrameData::Cpu(vec![Plane { data, stride: w * 4 }]),
            capture_ts: Timestamp::now(),
        };
        Ok((frame, t.elapsed().as_micros() as u32, 0))
    }
}

#[cfg(windows)]
/// GPU-resident synthetic source: pre-rendered ring of 4 D3D11 `BGRA8` textures on the
/// target adapter LUID, cycling zero-copy into `MfEncoder` with zero CPU→GPU upload per frame.
struct SyntheticGpu {
    w: u32,
    h: u32,
    luid: u64,
    ring: Vec<Arc<veloxdesk_codec::win::d3d::TextureHolder>>,
    n: usize,
}

#[cfg(windows)]
impl SyntheticGpu {
    fn new(w: u32, h: u32, luid: Option<u64>) -> Result<Self> {
        use windows::Win32::Graphics::Direct3D11::*;
        use windows::Win32::Graphics::Dxgi::Common::*;

        let dev = veloxdesk_codec::win::d3d::create_device(luid)
            .map_err(|e| anyhow!("create D3D11 device for SyntheticGpu: {e}"))?;
        let actual_luid = veloxdesk_codec::win::d3d::device_luid(&dev);
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut pool = veloxdesk_codec::win::d3d::TexturePool::new(desc, false, 4);
        let mut synth = Synthetic::new(w, h);
        let ctx = unsafe { dev.GetImmediateContext() }.map_err(|e| anyhow!("GetImmediateContext: {e}"))?;
        let mut ring = Vec::with_capacity(4);
        for _ in 0..4 {
            let (f, _, _) = synth.next()?;
            let FrameData::Cpu(ref planes) = f.data else { unreachable!() };
            let holder = pool.get(&dev).map_err(|e| anyhow!("TexturePool::get: {e}"))?;
            unsafe {
                ctx.UpdateSubresource(
                    &holder.tex,
                    0,
                    None,
                    planes[0].data.as_ptr().cast(),
                    planes[0].stride as u32,
                    0,
                );
            }
            ring.push(holder);
        }
        unsafe { ctx.Flush() };
        Ok(Self { w, h, luid: actual_luid, ring, n: 0 })
    }
}

#[cfg(windows)]
impl Source for SyntheticGpu {
    fn name(&self) -> String {
        format!("synthetic-gpu {}x{}", self.w, self.h)
    }
    fn next(&mut self) -> Result<(Frame, u32, u32)> {
        let t = Instant::now();
        let holder = &self.ring[self.n % self.ring.len()];
        self.n = self.n.wrapping_add(1);
        let frame = Frame {
            width: self.w,
            height: self.h,
            format: PixelFormat::Bgra8,
            color_space: ColorSpace::Srgb,
            color_range: ColorRange::Full,
            data: FrameData::Gpu(veloxdesk_codec::win::d3d::gpu_frame(holder, self.luid)),
            capture_ts: Timestamp::now(),
        };
        Ok((frame, t.elapsed().as_micros() as u32, 0))
    }
}

/// Real screen capture; repeats the previous frame when the screen is static.
struct Screen {
    cap: Box<dyn FrameCapture>,
    last: Option<Frame>,
    backend: &'static str,
}

impl Screen {
    fn new(args: &Args) -> Result<Self> {
        let mut cap = veloxdesk_capture::create_capture(args.gpu).map_err(|e| anyhow!("create_capture: {e}"))?;
        let displays = cap.displays().map_err(|e| anyhow!("displays: {e}"))?;
        let d = displays.iter().find(|d| d.primary).or(displays.first()).context("no displays")?;
        cap.start(&CaptureConfig {
            display: d.id.clone(),
            max_fps: args.fps,
            preferred_format: PixelFormat::Bgra8,
            prefer_gpu: args.gpu,
            draw_cursor: true,
            hdr: false,
        })
        .map_err(|e| anyhow!("capture start: {e}"))?;
        let backend = cap.caps().backend;
        Ok(Self { cap, last: None, backend })
    }
}

impl Source for Screen {
    fn name(&self) -> String {
        format!("capture ({})", self.backend)
    }
    fn next(&mut self) -> Result<(Frame, u32, u32)> {
        let timeout = Duration::from_millis(if self.last.is_some() { 5 } else { 500 });
        match self.cap.next_frame(timeout).map_err(|e| anyhow!("next_frame: {e}"))? {
            Some(c) => {
                self.last = Some(c.frame.clone());
                Ok((c.frame, c.capture_us, c.convert_us))
            }
            None => {
                let mut f = self.last.clone().context("static screen and no previous frame")?;
                f.capture_ts = Timestamp::now();
                Ok((f, 0, 0))
            }
        }
    }
}

// ───────────────────────────── main ─────────────────────────────

fn main() -> Result<()> {
    let args = Args::parse();
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build()?;
    let report = rt.block_on(run(args.clone()))?;
    println!("{report}");
    if let Some(p) = &args.report {
        std::fs::write(p, &report)?;
    }
    Ok(())
}

async fn run(args: Args) -> Result<String> {
    let codec = args.codec()?;
    let (stream_w, stream_h) = if args.synthetic {
        (args.width, args.height)
    } else {
        let displays = veloxdesk_capture::list_displays().map_err(|e| anyhow!("list_displays: {e}"))?;
        let d = displays.iter().find(|d| d.primary).or(displays.first()).context("no displays")?;
        (d.width, d.height)
    };
    let host_id = DeviceIdentity::generate();
    let viewer_id = DeviceIdentity::generate();

    let listener = veloxdesk_transport::listen("127.0.0.1:0".parse()?, &host_id).await.map_err(|e| anyhow!("listen: {e}"))?;
    let host_addr = listener.local_addr();
    let proxy = if args.impaired() {
        Some(UdpImpairProxy::start(host_addr, args.up_cfg(), args.down_cfg()).await?)
    } else {
        None
    };
    let connect_addr: SocketAddr = proxy.as_ref().map(|p| p.local_addr()).unwrap_or(host_addr);

    let (host_ch, viewer_ch) = tokio::join!(listener.accept(), veloxdesk_transport::connect(connect_addr, &viewer_id, None));
    let host_ch = host_ch.map_err(|e| anyhow!("accept: {e}"))?;
    let viewer_ch = viewer_ch.map_err(|e| anyhow!("connect: {e}"))?;
    let host_ch: Arc<dyn TransportChannel> = Arc::new(host_ch);
    let viewer_ch: Arc<dyn TransportChannel> = Arc::new(viewer_ch);

    let shared = Arc::new(Shared::default());

    // Viewer: datagrams → VideoReceiver → decode thread (latest-wins queue of 2).
    let (dec_tx, dec_rx) = bounded::<ViewerFrame>(2);
    let recv_task = {
        let shared = shared.clone();
        let ch = viewer_ch.clone();
        tokio::spawn(async move {
            let mut rx = VideoReceiver::new();
            rx.set_dimensions(stream_w, stream_h);
            let mut last_fb = Instant::now();
            let push_frame = |rf: veloxdesk_transport::ReceivedFrame| -> bool {
                shared.frames_complete.fetch_add(1, Ordering::Relaxed);
                shared.shards_recovered.fetch_add(rf.recovered_shards as u64, Ordering::Relaxed);
                let vf = ViewerFrame {
                    packet: rf.packet,
                    recv_complete: rf.recv_complete_ts.as_micros(),
                    recovered: rf.recovered_shards,
                };
                match dec_tx.try_send(vf) {
                    Ok(()) => true,
                    Err(TrySendError::Full(_)) => {
                        shared.dropped_latest_wins.fetch_add(1, Ordering::Relaxed);
                        true
                    }
                    Err(TrySendError::Disconnected(_)) => false,
                }
            };
            loop {
                match tokio::time::timeout(Duration::from_millis(5), ch.recv_datagram()).await {
                    Ok(Ok(d)) => {
                        let now = Timestamp::now();
                        if let Some(rf) = rx.push_datagram(&d, now)
                            && !push_frame(rf)
                        {
                            break;
                        }
                        while let Some(rf) = rx.pop_frame() {
                            if !push_frame(rf) {
                                return;
                            }
                        }
                    }
                    Ok(Err(_)) => break,
                    Err(_) => {
                        if shared.done.load(Ordering::Acquire) {
                            break;
                        }
                    }
                }
                if last_fb.elapsed() >= Duration::from_millis(5) {
                    last_fb = Instant::now();
                    let fb = rx.poll_feedback(Timestamp::now());
                    for env in &fb {
                        if matches!(env.msg, Some(veloxdesk_protocol::control::envelope::Msg::KeyframeRequest(_))) {
                            shared.pli_requested.store(true, Ordering::Release);
                        }
                    }
                    shared.feedback_msgs.fetch_add(fb.len() as u64, Ordering::Relaxed);
                    while let Some(rf) = rx.pop_frame() {
                        if !push_frame(rf) {
                            return;
                        }
                    }
                }
            }
        })
    };

    let use_hw = args.hw || args.zero_copy;
    let do_render = args.render || args.zero_copy;
    let dec_cfg = DecoderConfig {
        codec,
        width: stream_w,
        height: stream_h,
        prefer_gpu_output: args.zero_copy,
        shared_device: None,
    };
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let decode_thread = {
        let shared = shared.clone();
        thread::Builder::new()
            .name("decode".into())
            .spawn(move || decode_loop(dec_rx, dec_cfg, use_hw, do_render, ready_tx, shared))?
    };

    // Host: source → encode → VideoSender on a dedicated thread.
    let host_thread = {
        let shared = shared.clone();
        let mut host_args = args.clone();
        host_args.hw = use_hw;
        thread::Builder::new()
            .name("host".into())
            .spawn(move || host_loop(host_args, codec, host_ch, ready_rx, shared))?
    };

    let host_info = tokio::task::spawn_blocking(move || host_thread.join()).await?.map_err(|_| anyhow!("host thread panicked"))??;
    // Let the tail drain.
    tokio::time::sleep(Duration::from_millis(300 + (args.latency_ms * 2.0) as u64)).await;
    shared.done.store(true, Ordering::Release);
    recv_task.await?;
    let dec = tokio::task::spawn_blocking(move || decode_thread.join()).await?.map_err(|_| anyhow!("decode thread panicked"))??;

    let proxy_stats = proxy.as_ref().map(|p| p.stats(Direction::ToClient));
    Ok(build_report(&args, &host_info, &dec, &shared, proxy_stats, viewer_ch.stats().rtt_us))
}

struct HostInfo {
    source: String,
    encoder: String,
    hw: bool,
    duration: Duration,
    bytes: u64,
}

fn host_loop(
    args: Args,
    codec: VideoCodec,
    ch: Arc<dyn TransportChannel>,
    decoder_ready: std::sync::mpsc::Receiver<()>,
    shared: Arc<Shared>,
) -> Result<HostInfo> {
    let mut source: Box<dyn Source> = if args.synthetic {
        #[cfg(windows)]
        if args.gpu || args.zero_copy {
            Box::new(SyntheticGpu::new(args.width, args.height, None)?)
        } else {
            Box::new(Synthetic::new(args.width, args.height))
        }
        #[cfg(not(windows))]
        {
            Box::new(Synthetic::new(args.width, args.height))
        }
    } else {
        Box::new(Screen::new(&args)?)
    };
    let (first, first_cap_us, first_conv_us) = source.next()?;
    let cfg = EncoderConfig {
        codec,
        width: first.width,
        height: first.height,
        fps: args.fps,
        bitrate_kbps: args.bitrate_kbps,
        rate_control: RateControl::Cbr,
        chroma: ChromaFormat::Yuv420,
        bit_depth: 8,
        gop: None,
        intra_refresh: false,
        input_format: first.format,
        gpu_input: first.is_gpu(),
        color_space: ColorSpace::Bt709,
    };
    let mut enc = veloxdesk_codec::create_encoder(&cfg, args.hw).map_err(|e| anyhow!("create_encoder: {e}"))?;
    let caps = enc.caps();
    let mut sender = VideoSender::new(ch, 0);
    if args.parity > 0 {
        sender.set_parity_percent(args.parity);
    }
    // Wait until the viewer decoder & renderer have finished initializing so cold init
    // time does not pollute frame 0 latency or overflow the latest-wins queue.
    decoder_ready.recv().map_err(|_| anyhow!("decode thread exited before ready"))?;
    let interval = Duration::from_secs_f64(1.0 / args.fps.max(1) as f64);
    let start = Instant::now();
    let mut bytes = 0u64;
    let mut pending = Some((first, first_cap_us, first_conv_us));
    for i in 0..args.frames {
        let deadline = start + interval * i;
        spin_until(deadline);
        let (frame, capture_us, convert_cap_us) = match pending.take() {
            Some((mut f, cu, cv)) => {
                f.capture_ts = Timestamp::now();
                (f, cu, cv)
            }
            None => source.next()?,
        };
        let capture_ts = frame.capture_ts;
        let opts = EncodeOptions { force_keyframe: i == 0 || shared.pli_requested.swap(false, Ordering::AcqRel) };
        let Some(pkt) = enc.encode(&frame, opts).map_err(|e| anyhow!("encode: {e}"))? else { continue };
        let t = Instant::now();
        sender.send_frame(&pkt).map_err(|e| anyhow!("send_frame: {e}"))?;
        let packetize_us = t.elapsed().as_micros() as u32;
        bytes += pkt.data.len() as u64;
        shared.host.lock().expect("poisoned").insert(
            pkt.frame_id,
            HostRec {
                capture_ts: capture_ts.as_micros(),
                capture_us,
                convert_us: convert_cap_us + pkt.convert_us,
                encode_us: pkt.encode_us,
                packetize_us,
                sent_ts: Timestamp::now().as_micros(),
                bytes: pkt.data.len() as u32,
                keyframe: pkt.keyframe,
            },
        );
        shared.frames_sent.fetch_add(1, Ordering::Relaxed);
    }
    Ok(HostInfo { source: source.name(), encoder: caps.name.to_string(), hw: caps.hardware, duration: start.elapsed(), bytes })
}

fn spin_until(t: Instant) {
    loop {
        let now = Instant::now();
        if now >= t {
            return;
        }
        let left = t - now;
        if left > Duration::from_millis(4) {
            thread::sleep(left - Duration::from_millis(4));
        } else {
            thread::yield_now();
        }
    }
}

struct DecodeInfo {
    decoder: String,
    hw: bool,
    renderer: Option<String>,
    zero_copy_frames: u64,
    decoded: u64,
    errors: u64,
    /// (frame_id, recv_complete, decode_us, render_us, finished_ts, recovered)
    rows: Vec<(u32, u64, u32, Option<u32>, u64, u32)>,
}

fn decode_loop(
    rx: Receiver<ViewerFrame>,
    cfg: DecoderConfig,
    hw: bool,
    do_render: bool,
    ready: std::sync::mpsc::SyncSender<()>,
    _shared: Arc<Shared>,
) -> Result<DecodeInfo> {
    let mut dec = veloxdesk_codec::create_decoder(&cfg, hw).map_err(|e| anyhow!("create_decoder: {e}"))?;
    let caps = dec.caps();
    let mut offscreen = if do_render {
        let r = veloxdesk_render::OffscreenRenderer::new(cfg.width, cfg.height)
            .map_err(|e| anyhow!("OffscreenRenderer: {e}"))?;
        Some(r)
    } else {
        None
    };
    let _ = ready.send(());
    let renderer = offscreen.as_ref().map(|r| format!("{} ({:?})", r.adapter_name, r.backend));
    let mut info = DecodeInfo {
        decoder: caps.name.to_string(),
        hw: caps.hardware,
        renderer,
        zero_copy_frames: 0,
        decoded: 0,
        errors: 0,
        rows: Vec::new(),
    };
    while let Ok(vf) = rx.recv() {
        let t = Instant::now();
        match dec.decode(&vf.packet) {
            Ok(Some(df)) => {
                let decode_us = if df.decode_us > 0 { df.decode_us } else { t.elapsed().as_micros() as u32 };
                let render_us = if let Some(r) = offscreen.as_mut() {
                    let (us, zc) = r
                        .draw_frame(&df.frame, veloxdesk_core_traits::render::ScaleMode::Fit)
                        .map_err(|e| anyhow!("offscreen draw_frame: {e}"))?;
                    if zc {
                        info.zero_copy_frames += 1;
                    }
                    Some(us)
                } else {
                    None
                };
                info.decoded += 1;
                info.rows.push((
                    vf.packet.frame_id,
                    vf.recv_complete,
                    decode_us,
                    render_us,
                    Timestamp::now().as_micros(),
                    vf.recovered,
                ));
            }
            Ok(None) => {}
            Err(_) => {
                info.errors += 1;
                let _ = dec.reset();
            }
        }
    }
    Ok(info)
}

fn build_report(
    args: &Args,
    host: &HostInfo,
    dec: &DecodeInfo,
    shared: &Shared,
    proxy: Option<veloxdesk_benches::impair::DirStats>,
    rtt_us: u32,
) -> String {
    let recs = shared.host.lock().expect("poisoned");
    let mut capture = Stage::new("capture", None);
    let mut convert = Stage::new("convert", None);
    let mut cap_conv = Stage::new("capture+convert", Some(2_000));
    let mut encode = Stage::new("encode", Some(3_000));
    let mut packetize = Stage::new("packetize+FEC (send_frame)", None);
    let mut network = Stage::new("network (send→frame complete)", Some(3_000));
    let mut decode = Stage::new("decode", Some(2_000));
    let mut render = Stage::new("render (headless wgpu)", Some(2_000));
    let e2e_label = if dec.renderer.is_some() { "glass-to-glass (capture→rendered)" } else { "capture→decoded (no present)" };
    let e2e_budget = if dec.renderer.is_some() { Some(15_000) } else { Some(13_000) };
    let mut e2e = Stage::new(e2e_label, e2e_budget);
    for r in recs.values() {
        capture.push(r.capture_us);
        convert.push(r.convert_us);
        cap_conv.push(r.capture_us + r.convert_us);
        encode.push(r.encode_us);
        packetize.push(r.packetize_us);
    }
    for &(fid, recv_complete, decode_us, render_us, finished_ts, _rec) in &dec.rows {
        decode.push(decode_us);
        if let Some(ru) = render_us {
            render.push(ru);
        }
        if let Some(r) = recs.get(&fid) {
            network.push(recv_complete.saturating_sub(r.sent_ts.saturating_sub(r.packetize_us as u64)) as u32);
            // capture_ts is stamped at the end of capture; add capture_us for the full span.
            e2e.push((finished_ts.saturating_sub(r.capture_ts) + r.capture_us as u64) as u32);
        }
    }
    let sent = shared.frames_sent.load(Ordering::Relaxed);
    let complete = shared.frames_complete.load(Ordering::Relaxed);
    let secs = host.duration.as_secs_f64().max(1e-6);
    let keyframes = recs.values().filter(|r| r.keyframe).count();
    let max_frame = recs.values().map(|r| r.bytes).max().unwrap_or(0);
    let mut s = String::new();
    s.push_str("## VeloxDesk pipeline loopback\n\n");
    let render_note = dec.renderer.as_deref().map(|r| format!(" · renderer: {r}")).unwrap_or_default();
    s.push_str(&format!(
        "source: {} · encoder: {} ({}) · decoder: {} ({}){} · {:?} {} kbps target · {} fps target · parity {}%\n\n",
        host.source,
        host.encoder,
        if host.hw { "HW" } else { "SW" },
        dec.decoder,
        if dec.hw { "HW" } else { "SW" },
        render_note,
        args.codec().unwrap_or(VideoCodec::H264),
        args.bitrate_kbps,
        args.fps,
        args.parity
    ));
    let mut stages = vec![capture, convert, cap_conv, encode, packetize, network, decode];
    if dec.renderer.is_some() {
        stages.push(render);
    }
    stages.push(e2e);
    s.push_str(&latency_table(&stages));
    let zc_note = if dec.renderer.is_some() {
        format!(" · zero_copy {}/{}", dec.zero_copy_frames, dec.decoded)
    } else {
        String::new()
    };
    s.push_str(&format!(
        "\nframes: sent {sent}, complete {complete} ({:.2}%), decoded {} , decode errors {}, latest-wins drops {}{zc_note} · keyframes {keyframes} · max frame {} KB\n",
        100.0 * complete as f64 / sent.max(1) as f64,
        dec.decoded,
        dec.errors,
        shared.dropped_latest_wins.load(Ordering::Relaxed),
        max_frame / 1024
    ));
    s.push_str(&format!(
        "throughput: {:.1} fps sent, {:.2} Mb/s video · FEC recovered shards {} · feedback msgs {} · QUIC RTT {:.2} ms\n",
        sent as f64 / secs,
        host.bytes as f64 * 8.0 / secs / 1e6,
        shared.shards_recovered.load(Ordering::Relaxed),
        shared.feedback_msgs.load(Ordering::Relaxed),
        rtt_us as f64 / 1000.0
    ));
    if let Some(p) = proxy {
        s.push_str(&format!(
            "impairment host→viewer: {:?}, latency {:.1}±{:.1} ms · datagrams {} dropped(loss) {} dropped(queue) {} → {:.2}% loss\n",
            args.down_cfg().loss,
            args.latency_ms,
            args.jitter_ms,
            p.received,
            p.dropped_loss,
            p.dropped_queue,
            100.0 * p.loss_rate()
        ));
    }
    s
}
