//! Host video pipeline: **capture thread** → latest-frame-wins slot → **encode thread**
//! → [`VideoSink`] (FEC datagrams). Both threads are plain OS threads (ADR-006); the
//! async session talks to them through [`VideoPipeline`] and receives [`VideoEvent`]s.

use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, TryRecvError};
use tokio::sync::mpsc::UnboundedSender;
use veloxdesk_core_traits::video::{
    CaptureConfig, CapturedFrame, ChromaFormat, Display, EncodeOptions, EncoderConfig, EncoderUpdate, Frame,
    PixelFormat, RateControl, VideoCodec,
};
use veloxdesk_core_traits::{CoreError, FrameCapture, Timestamp, TransportChannel, VideoEncoder};
use veloxdesk_protocol::control::{CursorPosition, CursorShape, FrameTimingReport, Nack, VideoConfig};

use crate::backends::{HostBackends, RateDecision, VideoSink};
use crate::latest::{LatestSlot, Wake};

/// Static parameters of one video stream.
#[derive(Debug, Clone)]
pub struct VideoParams {
    pub stream_id: u8,
    /// Requested display index (`None` = primary).
    pub display: Option<u32>,
    pub max_fps: u32,
    pub bitrate_kbps: u32,
    pub codec: VideoCodec,
    pub prefer_hw: bool,
    pub prefer_gpu: bool,
    /// Composite the cursor into the image (desktop mode); otherwise it is sent as
    /// `CursorPosition` / `CursorShape` (gaming mode).
    pub draw_cursor: bool,
    /// Static-screen refresh rate (re-encode of the last frame); 0 = off.
    pub idle_fps: u32,
    pub parity_percent: u8,
    /// Enable 10-bit HDR (P010 / HEVC Main10 / AV1 10-bit) when supported.
    pub ten_bit: bool,
    /// Preferred chroma format (`Yuv420` or `Yuv444`).
    pub chroma: ChromaFormat,
}

/// Events from the pipeline threads to the session task.
#[derive(Debug)]
pub enum VideoEvent {
    /// Display list and the one being captured (sent at start and after a switch).
    Displays { displays: Vec<Display>, active: u32 },
    /// Encoder (re)created — forward to the viewer (`Envelope::VideoConfig`).
    Config(VideoConfig),
    Timing(FrameTimingReport),
    Cursor(CursorPosition),
    CursorShape(CursorShape),
    /// The pipeline cannot run (no capture device, no encoder at all).
    Fatal(String),
}

pub(crate) enum EncodeCmd {
    Keyframe,
    /// Clear cached last frame (display switched or capture reinitialized) and force IDR on next frame.
    ResetLast,
    Rate(RateDecision),
    Nack(Nack),
}

enum CaptureCmd {
    SwitchDisplay(u32),
}

/// Handle owned by the session.
pub struct VideoPipeline {
    slot: LatestSlot<CapturedFrame, EncodeCmd>,
    cap_tx: Sender<CaptureCmd>,
    threads: Vec<JoinHandle<()>>,
}

impl VideoPipeline {
    pub fn start(
        params: VideoParams,
        backends: &HostBackends,
        channel: Arc<dyn TransportChannel>,
        events: UnboundedSender<VideoEvent>,
    ) -> std::io::Result<Self> {
        let slot = LatestSlot::new();
        let (cap_tx, cap_rx) = crossbeam_channel::unbounded();
        let sink = (backends.video_sink)(channel, params.stream_id);

        let cap = CaptureLoop {
            params: params.clone(),
            factory: backends.capture.clone(),
            slot: slot.clone(),
            ctl: cap_rx,
            events: events.clone(),
        };
        let enc = EncodeLoop {
            params,
            factory: backends.encoder.clone(),
            slot: slot.clone(),
            sink,
            events,
            encoder: None,
            bitrate_kbps: 0,
            last: None,
            failures: 0,
            hw_allowed: true,
            dims: (0, 0),
        };
        let t1 = std::thread::Builder::new().name("vdk-capture".into()).spawn(move || cap.run())?;
        let t2 = std::thread::Builder::new().name("vdk-encode".into()).spawn(move || enc.run())?;
        Ok(Self { slot, cap_tx, threads: vec![t1, t2] })
    }

    pub fn request_keyframe(&self) {
        self.slot.command(EncodeCmd::Keyframe);
    }

    pub fn apply_rate(&self, d: RateDecision) {
        self.slot.command(EncodeCmd::Rate(d));
    }

    pub fn nack(&self, n: Nack) {
        self.slot.command(EncodeCmd::Nack(n));
    }

    pub fn switch_display(&self, index: u32) {
        let _ = self.cap_tx.send(CaptureCmd::SwitchDisplay(index));
    }

    /// Frames overwritten in the slot because the encoder was busy.
    pub fn dropped_frames(&self) -> u64 {
        self.slot.dropped()
    }

    /// Stop both threads and wait for them (blocking; call from `spawn_blocking`).
    pub fn stop(mut self) {
        self.slot.close();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

impl Drop for VideoPipeline {
    fn drop(&mut self) {
        self.slot.close();
    }
}

// ───────────────────────────── capture ─────────────────────────────

struct CaptureLoop {
    params: VideoParams,
    factory: Arc<crate::backends::CaptureFactory>,
    slot: LatestSlot<CapturedFrame, EncodeCmd>,
    ctl: Receiver<CaptureCmd>,
    events: UnboundedSender<VideoEvent>,
}

fn pick_display(displays: &[Display], want: Option<u32>) -> Option<&Display> {
    want.and_then(|i| displays.iter().find(|d| d.index == i))
        .or_else(|| displays.iter().find(|d| d.primary))
        .or_else(|| displays.first())
}

impl CaptureLoop {
    /// Create + start a capture for `want`; announces the display list.
    fn open(&self, want: Option<u32>) -> veloxdesk_core_traits::Result<(Box<dyn FrameCapture>, CaptureConfig)> {
        let mut cap = (self.factory)(self.params.prefer_gpu)?;
        let displays = cap.displays()?;
        let d = pick_display(&displays, want).ok_or_else(|| CoreError::unsupported("no displays"))?.clone();
        let cfg = CaptureConfig {
            display: d.id.clone(),
            max_fps: self.params.max_fps,
            preferred_format: if self.params.ten_bit { PixelFormat::Rgb10a2 } else { PixelFormat::Bgra8 },
            prefer_gpu: self.params.prefer_gpu,
            draw_cursor: self.params.draw_cursor,
            hdr: self.params.ten_bit,
        };
        cap.start(&cfg)?;
        tracing::info!(display = d.index, name = %d.name, w = d.width, h = d.height, hz = d.refresh_hz,
            backend = cap.caps().backend, "capture started");
        let _ = self.events.send(VideoEvent::Displays { displays, active: d.index });
        Ok((cap, cfg))
    }

    fn run(self) {
        let interval = Duration::from_micros(1_000_000 / self.params.max_fps.max(1) as u64);
        let mut want = self.params.display;
        let (mut cap, mut cfg) = match self.open(want) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("capture init failed: {e}");
                let _ = self.events.send(VideoEvent::Fatal(format!("capture: {e}")));
                self.slot.close();
                return;
            }
        };
        let mut next_due = Instant::now();
        let mut backoff = Duration::from_millis(50);
        let mut last_cursor: Option<(i32, i32, bool)> = None;
        let mut last_shape_id: u64 = 0;
        let mut have_image = false;
        while !self.slot.is_closed() {
            match self.ctl.try_recv() {
                Ok(CaptureCmd::SwitchDisplay(i)) => {
                    cap.stop();
                    want = Some(i);
                    have_image = false;
                    match self.open(want) {
                        Ok(v) => (cap, cfg) = v,
                        Err(e) => {
                            tracing::warn!("switch to display {i} failed: {e}");
                            if let Err(e) = cap.start(&cfg) {
                                tracing::warn!("restart of previous display failed: {e}");
                            }
                        }
                    }
                    self.slot.command(EncodeCmd::ResetLast);
                }
                Err(TryRecvError::Disconnected) => break,
                Err(TryRecvError::Empty) => {}
            }
            // Pace to max_fps by delaying the *acquire*: the OS accumulates updates, so
            // the frame we get is the freshest one (no stale-frame drops).
            let now = Instant::now();
            if now < next_due {
                std::thread::sleep(next_due - now);
            }
            match cap.next_frame(Duration::from_millis(50)) {
                Ok(Some(frame)) => {
                    backoff = Duration::from_millis(50);
                    self.forward_cursor(&frame, &mut last_cursor, &mut last_shape_id);
                    let cursor_only = frame.dirty_rects.len() == 1
                        && frame.dirty_rects[0].width == 0
                        && frame.dirty_rects[0].height == 0;
                    if cursor_only && have_image {
                        // Image pixels are unchanged; avoid waking the encoder for cursor motion.
                        continue;
                    }
                    have_image = true;
                    let t = Instant::now();
                    next_due = (next_due + interval).max(t + interval.mul_f32(0.5));
                    if !self.slot.put(frame) {
                        break;
                    }
                }
                Ok(None) => {}
                Err(CoreError::Timeout) => {}
                Err(e) => {
                    tracing::warn!("capture error: {e}; reinitialising in {backoff:?}");
                    std::thread::sleep(backoff);
                    backoff = (backoff * 2).min(Duration::from_secs(1));
                    cap.stop();
                    let restarted = matches!(e, CoreError::Reinit(_)) && cap.start(&cfg).is_ok();
                    if !restarted {
                        match self.open(want) {
                            Ok(v) => (cap, cfg) = v,
                            Err(e2) => tracing::warn!("capture re-open failed: {e2}"),
                        }
                    }
                    self.slot.command(EncodeCmd::ResetLast);
                }
            }
        }
        cap.stop();
        tracing::debug!("capture thread exit");
    }

    fn forward_cursor(&self, frame: &CapturedFrame, last: &mut Option<(i32, i32, bool)>, last_shape_id: &mut u64) {
        let Some(c) = &frame.cursor else { return };
        if let Some(shape) = &c.shape {
            *last_shape_id = shape.id;
            let _ = self.events.send(VideoEvent::CursorShape(CursorShape {
                id: shape.id,
                width: shape.width,
                height: shape.height,
                hotspot_x: shape.hotspot_x,
                hotspot_y: shape.hotspot_y,
                rgba: shape.rgba.clone(),
            }));
        }
        let cur = (c.x, c.y, c.visible);
        if *last != Some(cur) || c.shape.is_some() {
            *last = Some(cur);
            let _ = self.events.send(VideoEvent::Cursor(CursorPosition {
                x: c.x,
                y: c.y,
                visible: c.visible,
                shape_id: *last_shape_id,
            }));
        }
    }
}

// ───────────────────────────── encode ─────────────────────────────

struct LastFrame {
    frame: Frame,
}

struct EncodeLoop {
    params: VideoParams,
    factory: Arc<crate::backends::EncoderFactory>,
    slot: LatestSlot<CapturedFrame, EncodeCmd>,
    sink: Box<dyn VideoSink>,
    events: UnboundedSender<VideoEvent>,
    encoder: Option<Box<dyn VideoEncoder>>,
    bitrate_kbps: u32,
    last: Option<LastFrame>,
    failures: u32,
    hw_allowed: bool,
    dims: (u32, u32),
}

impl EncodeLoop {
    fn run(mut self) {
        self.bitrate_kbps = self.params.bitrate_kbps;
        self.sink.set_parity_percent(self.params.parity_percent);
        let idle = (self.params.idle_fps > 0).then(|| Duration::from_micros(1_000_000 / self.params.idle_fps as u64));
        let mut force_kf = true;
        let mut last_encode = Instant::now();
        loop {
            let timeout = match idle {
                Some(iv) => iv.saturating_sub(last_encode.elapsed()),
                None => Duration::from_secs(1),
            };
            match self.slot.wait(timeout) {
                Wake::Closed => break,
                Wake::Cmd(EncodeCmd::Keyframe) => {
                    force_kf = true;
                    // Answer PLI right away with the last image instead of waiting for
                    // the next screen change.
                    if self.reencode_last(true) {
                        force_kf = false;
                        last_encode = Instant::now();
                    }
                }
                Wake::Cmd(EncodeCmd::ResetLast) => {
                    self.last = None;
                    force_kf = true;
                }
                Wake::Cmd(EncodeCmd::Rate(d)) => {
                    self.sink.set_parity_percent(d.parity_percent);
                    if d.bitrate_kbps != self.bitrate_kbps {
                        self.bitrate_kbps = d.bitrate_kbps;
                        if let Some(enc) = self.encoder.as_mut() {
                            let upd = EncoderUpdate { bitrate_kbps: Some(d.bitrate_kbps), fps: None };
                            if let Err(e) = enc.update(upd) {
                                tracing::warn!("encoder bitrate update failed: {e}");
                            }
                        }
                    }
                }
                Wake::Cmd(EncodeCmd::Nack(n)) => self.sink.on_nack(&n),
                Wake::Item(cf) => {
                    let convert = cf.convert_us;
                    if self.encode(&cf.frame, cf.capture_us, convert, force_kf) {
                        force_kf = false;
                    }
                    last_encode = Instant::now();
                    self.last = Some(LastFrame { frame: cf.frame });
                }
                Wake::Timeout => {
                    if idle.is_some() && self.reencode_last(force_kf) {
                        force_kf = false;
                    }
                    last_encode = Instant::now();
                }
            }
        }
        tracing::debug!("encode thread exit");
    }

    /// Re-encode the last captured image stamped "now" (static screen / PLI answer).
    fn reencode_last(&mut self, keyframe: bool) -> bool {
        let Some(last) = self.last.take() else { return false };
        let mut frame = last.frame;
        frame.capture_ts = Timestamp::now();
        let ok = self.encode(&frame, 0, 0, keyframe);
        self.last = Some(LastFrame { frame });
        ok
    }

    fn ensure_encoder(&mut self, frame: &Frame) -> bool {
        if self.encoder.is_some() && self.dims == (frame.width, frame.height) {
            return true;
        }
        self.encoder = None;
        let want_depth = if self.params.ten_bit { 10u8 } else { 8u8 };
        let mut attempts = vec![(self.params.codec, self.params.chroma, want_depth)];
        if want_depth != 8 || self.params.chroma != ChromaFormat::Yuv420 {
            attempts.push((self.params.codec, ChromaFormat::Yuv420, 8));
        }
        if self.params.codec != VideoCodec::H264 {
            attempts.push((VideoCodec::H264, ChromaFormat::Yuv420, 8));
        }
        for (codec, chroma, bit_depth) in attempts {
            let cfg = EncoderConfig {
                codec,
                width: frame.width,
                height: frame.height,
                fps: self.params.max_fps,
                bitrate_kbps: self.bitrate_kbps,
                rate_control: RateControl::Cbr,
                chroma,
                bit_depth,
                gop: None,
                intra_refresh: false,
                input_format: frame.format,
                gpu_input: frame.is_gpu(),
                color_space: frame.color_space,
            };
            match (self.factory)(&cfg, self.params.prefer_hw && self.hw_allowed) {
                Ok(enc) => {
                    let caps = enc.caps();
                    let zero_copy = frame.is_gpu() && caps.gpu_input_formats.contains(&frame.format);
                    tracing::info!(encoder = caps.name, hw = caps.hardware, ?codec, ?chroma, bit_depth,
                        w = frame.width, h = frame.height, zero_copy, "encoder ready");
                    let _ = self.events.send(VideoEvent::Config(VideoConfig {
                        codec: codec as i32,
                        width: frame.width,
                        height: frame.height,
                        fps: self.params.max_fps,
                        bitrate_kbps: self.bitrate_kbps,
                        chroma: chroma as i32,
                        bit_depth: bit_depth as u32,
                        encoder_name: caps.name.to_string(),
                        hw_encoder: caps.hardware,
                        zero_copy,
                        fec_parity_percent: self.params.parity_percent as u32,
                        stream_id: self.params.stream_id as u32,
                    }));
                    self.encoder = Some(enc);
                    self.dims = (frame.width, frame.height);
                    return true;
                }
                Err(e) => tracing::warn!(?codec, ?chroma, bit_depth, "encoder creation failed: {e}"),
            }
        }
        let _ = self.events.send(VideoEvent::Fatal("no usable video encoder".into()));
        false
    }

    /// Encode one frame and hand it to the sink. Returns `true` if a packet was sent.
    fn encode(&mut self, frame: &Frame, capture_us: u32, capture_convert_us: u32, keyframe: bool) -> bool {
        let fresh = self.encoder.is_none() || self.dims != (frame.width, frame.height);
        if !self.ensure_encoder(frame) {
            return false;
        }
        let Some(enc) = self.encoder.as_mut() else { return false };
        let opts = EncodeOptions { force_keyframe: keyframe || fresh };
        match enc.encode(frame, opts) {
            Ok(Some(pkt)) => {
                self.failures = 0;
                let packetize_us = match self.sink.send_frame(&pkt) {
                    Ok(us) => us,
                    Err(CoreError::Closed) => {
                        self.slot.close();
                        return false;
                    }
                    Err(e) => {
                        tracing::debug!("video send failed: {e}");
                        return false;
                    }
                };
                let _ = self.events.send(VideoEvent::Timing(FrameTimingReport {
                    stream_id: self.params.stream_id as u32,
                    frame_id: pkt.frame_id,
                    capture_ts_us: pkt.capture_ts.as_micros(),
                    capture_us,
                    convert_us: capture_convert_us + pkt.convert_us,
                    encode_us: pkt.encode_us,
                    packetize_us,
                    size_bytes: pkt.data.len() as u32,
                }));
                true
            }
            Ok(None) => false,
            Err(e) => {
                self.failures += 1;
                tracing::warn!(failures = self.failures, "encode failed: {e}");
                if matches!(e, CoreError::Reinit(_)) || self.failures >= 3 {
                    if self.failures >= 3 && self.hw_allowed && self.params.prefer_hw {
                        tracing::warn!("falling back to software encoder");
                        self.hw_allowed = false;
                    }
                    self.encoder = None;
                    self.failures = 0;
                }
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use veloxdesk_core_traits::video::DisplayId;

    fn disp(index: u32, primary: bool) -> Display {
        Display {
            id: DisplayId(format!("D{index}")),
            index,
            name: String::new(),
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            refresh_hz: 60,
            scale_factor: 1.0,
            primary,
            hdr: false,
        }
    }

    #[test]
    fn display_choice() {
        let ds = vec![disp(0, false), disp(1, true), disp(2, false)];
        assert_eq!(pick_display(&ds, None).unwrap().index, 1);
        assert_eq!(pick_display(&ds, Some(2)).unwrap().index, 2);
        assert_eq!(pick_display(&ds, Some(9)).unwrap().index, 1);
        assert!(pick_display(&[], None).is_none());
    }
}
