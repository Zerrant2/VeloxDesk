//! Viewer network + decode session runner (ADR-006: tokio for QUIC/control, dedicated OS
//! thread for video decoding, "latest frame wins" handoff to the winit/wgpu render thread).

#[cfg(feature = "net")]
pub use net::{connect, device_id_label, load_identity, set_identity_override};

#[cfg(not(feature = "net"))]
pub fn set_identity_override(_path: std::path::PathBuf) {}

#[cfg(not(feature = "net"))]
pub fn device_id_label() -> String {
    String::new()
}

#[cfg(not(feature = "net"))]
pub fn connect(
    opts: crate::session::ConnectOptions,
    _proxy: veloxdesk_render::winit::event_loop::EventLoopProxy<crate::app::UserEvent>,
) -> anyhow::Result<crate::session::SessionHandle> {
    anyhow::bail!("{}", veloxdesk_ui::tr(opts.lang()).err_net_feature_disabled)
}

#[cfg(feature = "net")]
mod net {
    use std::collections::HashMap;
    use std::net::SocketAddr;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, OnceLock};
    use std::time::{Duration, Instant};

    use anyhow::{Context, bail};
    use crossbeam_channel::{Receiver, Sender, TrySendError};
    use tokio::sync::{mpsc, watch};
    use tokio::time::{interval, timeout};
    use veloxdesk_audio::{AudioJitterBuffer, JitterConfig, OpusDecoder, OpusEncoder};
    use veloxdesk_core_traits::audio::{
        AudioCaptureConfig, AudioCodecId, AudioEncoder, AudioEncoderConfig, AudioFormat, EncodedAudio,
    };
    use veloxdesk_core_traits::video::{CursorImage, DecoderConfig, VideoCodec, VideoDecoder};
    use veloxdesk_core_traits::{CoreError, Timestamp, TransportChannel};
    use veloxdesk_crypto::DeviceIdentity;
    use veloxdesk_crypto::auth::{Argon2Params, derive_password_key, password_proof, sign_pubkey_challenge};
    use veloxdesk_file_transfer::FileTransferClient;
    use veloxdesk_protocol::control::{
        AudioCodec, AudioConfig, AuthChallenge, AuthMethod, AuthResponse, BitrateUpdate, Close, Envelope, GamepadState,
        Hello, InputEvent, Ping, PrivacyMode, SessionConfig, SwitchDisplay, VideoCodecCap, VideoConfig, envelope::Msg,
    };
    use veloxdesk_protocol::media::MediaKind;
    use veloxdesk_protocol::{PROTOCOL_VERSION, StreamKind};
    use veloxdesk_render::winit::event_loop::EventLoopProxy;
    use veloxdesk_terminal::{TerminalClient, TerminalEvent};
    use veloxdesk_transport::{
        AudioReceiver, AudioSender, ControlSender, ReceivedFrame, VideoReceiver, datagram_kind,
        encode_gamepad_datagram, framed,
    };
    use veloxdesk_tunnel::LocalForwarder;
    use veloxdesk_ui::{
        ActiveTunnel, ConnectTarget, FileAction, Language, TerminalAction, TransferProgress, TunnelAction, TunnelSpec,
        format_device_id, tr,
    };

    use crate::app::UserEvent;
    use crate::gamepad::GamepadPoller;
    use crate::session::{ConnectOptions, DecodedMsg, SessionCommand, SessionEvent, SessionHandle, SessionShared};

    const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
    const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
    /// Generous wait for an interactive host Accept dialog after `AuthResponse`.
    const APPROVAL_TIMEOUT: Duration = Duration::from_secs(60);
    const FEEDBACK_TICK: Duration = Duration::from_millis(5);
    const PING_TICK: Duration = Duration::from_millis(500);
    const DECODER_QUEUE_DEPTH: usize = 4;

    static IDENTITY_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

    /// Override the default persisted identity key path (used by `--portable` mode).
    pub fn set_identity_override(path: PathBuf) {
        let _ = IDENTITY_OVERRIDE.set(path);
    }

    /// Default path for the persisted Ed25519 identity key.
    fn default_identity_path() -> Option<PathBuf> {
        IDENTITY_OVERRIDE
            .get()
            .cloned()
            .or_else(|| veloxdesk_ui::settings::config_dir().map(|d| d.join("veloxdesk").join("identity.key")))
    }

    /// Load the device identity from `override_path` or the per-user config directory
    /// (falling back to an ephemeral key if no config directory is available).
    pub fn load_identity(override_path: Option<&Path>) -> anyhow::Result<DeviceIdentity> {
        let path = override_path.map(Path::to_path_buf).or_else(default_identity_path);
        match path {
            Some(p) => DeviceIdentity::load_or_create(&p)
                .with_context(|| format!("loading device identity at {}", p.display())),
            None => Ok(DeviceIdentity::generate()),
        }
    }

    /// Cached 9-digit device ID shown on the connect screen.
    pub fn device_id_label() -> String {
        static LABEL: OnceLock<String> = OnceLock::new();
        LABEL
            .get_or_init(|| match load_identity(None) {
                Ok(id) => id.device_id(),
                Err(e) => {
                    tracing::warn!("device identity unavailable: {e:#}");
                    String::new()
                }
            })
            .clone()
    }

    /// Start a viewer session in the background; returns a handle for commands and decoded frames.
    pub fn connect(opts: ConnectOptions, proxy: EventLoopProxy<UserEvent>) -> anyhow::Result<SessionHandle> {
        let shared = Arc::new(SessionShared::default());
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let shared_bg = shared.clone();
        std::thread::Builder::new()
            .name("viewer-net".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .thread_name("viewer-tokio")
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        let _ = proxy.send_event(UserEvent::Session(SessionEvent::Closed(
                            tr(opts.lang()).fmt_err_async_runtime(&e.to_string()),
                        )));
                        let _ = done_tx.send(());
                        return;
                    }
                };
                let closed_reason = rt.block_on(run_viewer_session(opts, shared_bg, cmd_rx, proxy.clone()));
                if let Some(reason) = closed_reason {
                    let _ = proxy.send_event(UserEvent::Session(SessionEvent::Closed(reason)));
                }
                let _ = done_tx.send(());
            })
            .context("spawning viewer network thread")?;
        Ok(SessionHandle { shared, cmd: cmd_tx, done: Some(done_rx) })
    }

    enum DecoderMsg {
        Configure(VideoConfig),
        Frame(ReceivedFrame),
        Stop,
    }

    struct DecoderHandle {
        tx: Sender<DecoderMsg>,
        rx_peek: Receiver<DecoderMsg>,
        need_keyframe: Arc<AtomicBool>,
        pli_requested: Arc<AtomicBool>,
        join: Option<std::thread::JoinHandle<()>>,
    }

    impl DecoderHandle {
        fn spawn(
            prefer_hw: bool,
            prefer_gpu: bool,
            shared: Arc<SessionShared>,
            proxy: EventLoopProxy<UserEvent>,
        ) -> anyhow::Result<Self> {
            let (tx, rx) = crossbeam_channel::bounded::<DecoderMsg>(DECODER_QUEUE_DEPTH);
            let rx_peek = rx.clone();
            let need_keyframe = Arc::new(AtomicBool::new(true));
            let pli_requested = Arc::new(AtomicBool::new(false));
            let need_kf = need_keyframe.clone();
            let pli = pli_requested.clone();
            let join = std::thread::Builder::new()
                .name("viewer-decoder".into())
                .spawn(move || decoder_loop(rx, prefer_hw, prefer_gpu, shared, proxy, need_kf, pli))
                .context("spawning viewer decoder thread")?;
            Ok(Self { tx, rx_peek, need_keyframe, pli_requested, join: Some(join) })
        }

        fn configure(&self, cfg: VideoConfig) {
            let _ = self.tx.send(DecoderMsg::Configure(cfg));
        }

        /// Push a reassembled frame to the decoder thread without blocking the network loop.
        /// Returns `false` if the frame had to be dropped because the decoder fell behind.
        fn push_frame(&self, rf: ReceivedFrame) -> bool {
            let is_key = rf.packet.keyframe;
            match self.tx.try_send(DecoderMsg::Frame(rf)) {
                Ok(()) => true,
                Err(TrySendError::Full(DecoderMsg::Frame(rf))) => {
                    if is_key {
                        // A keyframe supersedes every older frame queued for decode.
                        while let Ok(item) = self.rx_peek.try_recv() {
                            match item {
                                DecoderMsg::Configure(cfg) => {
                                    let _ = self.tx.try_send(DecoderMsg::Configure(cfg));
                                    break;
                                }
                                DecoderMsg::Stop => {
                                    let _ = self.tx.try_send(DecoderMsg::Stop);
                                    return false;
                                }
                                DecoderMsg::Frame(_) => {}
                            }
                        }
                        self.tx.try_send(DecoderMsg::Frame(rf)).is_ok()
                    } else {
                        tracing::debug!(frame_id = rf.packet.frame_id, "decoder queue full, dropping delta frame");
                        self.need_keyframe.store(true, Ordering::Relaxed);
                        self.pli_requested.store(true, Ordering::Relaxed);
                        false
                    }
                }
                Err(_) => false,
            }
        }

        fn take_pli_request(&self) -> bool {
            self.pli_requested.swap(false, Ordering::Relaxed)
        }

        fn stop(mut self) {
            let _ = self.tx.send(DecoderMsg::Stop);
            if let Some(j) = self.join.take() {
                let _ = j.join();
            }
        }
    }

    impl Drop for DecoderHandle {
        fn drop(&mut self) {
            let _ = self.tx.try_send(DecoderMsg::Stop);
            if let Some(j) = self.join.take() {
                let _ = j.join();
            }
        }
    }

    fn decoder_loop(
        rx: Receiver<DecoderMsg>,
        prefer_hw: bool,
        prefer_gpu: bool,
        shared: Arc<SessionShared>,
        proxy: EventLoopProxy<UserEvent>,
        need_keyframe: Arc<AtomicBool>,
        pli_requested: Arc<AtomicBool>,
    ) {
        let mut decoder: Option<Box<dyn VideoDecoder>> = None;
        let mut active_cfg: Option<(VideoCodec, u32, u32)> = None;

        while let Ok(msg) = rx.recv() {
            match msg {
                DecoderMsg::Stop => break,
                DecoderMsg::Configure(vcfg) => {
                    let codec = VideoCodec::try_from(vcfg.codec).unwrap_or(VideoCodec::H264);
                    if codec == VideoCodec::Unspecified || vcfg.width == 0 || vcfg.height == 0 {
                        continue;
                    }
                    let key = (codec, vcfg.width, vcfg.height);
                    if active_cfg == Some(key) && decoder.is_some() {
                        continue;
                    }
                    match open_decoder(codec, vcfg.width, vcfg.height, prefer_hw, prefer_gpu, &proxy) {
                        Ok(dec) => {
                            decoder = Some(dec);
                            active_cfg = Some(key);
                            need_keyframe.store(true, Ordering::Relaxed);
                        }
                        Err(e) => {
                            tracing::error!("cannot create video decoder for {codec:?}: {e}");
                        }
                    }
                }
                DecoderMsg::Frame(rf) => {
                    if decoder.is_none()
                        && rf.packet.codec != VideoCodec::Unspecified
                        && rf.packet.width > 0
                        && rf.packet.height > 0
                    {
                        match open_decoder(
                            rf.packet.codec,
                            rf.packet.width,
                            rf.packet.height,
                            prefer_hw,
                            prefer_gpu,
                            &proxy,
                        ) {
                            Ok(dec) => {
                                active_cfg = Some((rf.packet.codec, rf.packet.width, rf.packet.height));
                                decoder = Some(dec);
                                need_keyframe.store(true, Ordering::Relaxed);
                            }
                            Err(e) => {
                                tracing::warn!("lazy decoder creation failed: {e}");
                                continue;
                            }
                        }
                    }
                    let Some(dec) = decoder.as_mut() else { continue };

                    if rf.discontinuity && !rf.packet.keyframe {
                        let _ = dec.reset();
                        need_keyframe.store(true, Ordering::Relaxed);
                        pli_requested.store(true, Ordering::Relaxed);
                        continue;
                    }

                    if need_keyframe.load(Ordering::Relaxed) {
                        if rf.packet.keyframe || rf.packet.recovery_point {
                            need_keyframe.store(false, Ordering::Relaxed);
                        } else {
                            pli_requested.store(true, Ordering::Relaxed);
                            continue;
                        }
                    }

                    match dec.decode(&rf.packet) {
                        Ok(Some(mut decoded)) => {
                            if decoded.frame.capture_ts.0 == 0 && rf.packet.capture_ts.0 != 0 {
                                decoded.frame.capture_ts = rf.packet.capture_ts;
                            }
                            if decoded.frame_id == 0 && rf.packet.frame_id != 0 {
                                decoded.frame_id = rf.packet.frame_id;
                            }
                            shared.decode_us.store(decoded.decode_us, Ordering::Relaxed);
                            let msg = DecodedMsg {
                                frame: decoded.frame,
                                frame_id: decoded.frame_id,
                                decode_us: decoded.decode_us,
                                recv_complete: rf.recv_complete_ts,
                            };
                            if shared.frames.put(msg) {
                                let _ = proxy.send_event(UserEvent::Frame);
                            }
                        }
                        Ok(None) => {}
                        Err(e) => {
                            tracing::warn!(frame_id = rf.packet.frame_id, "decode error (requesting PLI): {e}");
                            let _ = dec.reset();
                            need_keyframe.store(true, Ordering::Relaxed);
                            pli_requested.store(true, Ordering::Relaxed);
                        }
                    }
                }
            }
        }
    }

    fn open_decoder(
        codec: VideoCodec,
        width: u32,
        height: u32,
        prefer_hw: bool,
        prefer_gpu: bool,
        proxy: &EventLoopProxy<UserEvent>,
    ) -> veloxdesk_core_traits::Result<Box<dyn VideoDecoder>> {
        let cfg = DecoderConfig {
            codec,
            width,
            height,
            prefer_gpu_output: prefer_hw && prefer_gpu,
            shared_device: None,
        };
        let dec = veloxdesk_codec::create_decoder(&cfg, prefer_hw)?;
        let caps = dec.caps();
        tracing::info!(
            decoder = caps.name,
            hardware = caps.hardware,
            gpu_output = caps.gpu_output && cfg.prefer_gpu_output,
            ?codec,
            width,
            height,
            "video decoder ready"
        );
        let _ = proxy.send_event(UserEvent::Session(SessionEvent::Decoder {
            name: caps.name.to_string(),
            hardware: caps.hardware,
        }));
        Ok(dec)
    }

    // ───────────────────────── Audio playback & mic threads (M3) ─────────────────────────

    struct AudioPlaybackHandle {
        tx: Sender<EncodedAudio>,
        enabled: Arc<AtomicBool>,
        stop: Arc<AtomicBool>,
        join: Option<std::thread::JoinHandle<()>>,
    }

    impl AudioPlaybackHandle {
        fn spawn(cfg: Option<&AudioConfig>, initial_enabled: bool, shared: Arc<SessionShared>) -> Option<Self> {
            let fmt = cfg
                .map(|c| AudioFormat {
                    sample_rate: c.sample_rate.max(8_000),
                    channels: c.channels.clamp(1, 2) as u16,
                })
                .unwrap_or(AudioFormat::STEREO_48K);
            let frame_us = cfg.map(|c| c.frame_duration_us).filter(|&u| u >= 2_500).unwrap_or(5_000);
            let (tx, rx) = crossbeam_channel::bounded::<EncodedAudio>(64);
            let enabled = Arc::new(AtomicBool::new(initial_enabled));
            let stop = Arc::new(AtomicBool::new(false));
            let enabled_bg = enabled.clone();
            let stop_bg = stop.clone();
            let join = std::thread::Builder::new()
                .name("viewer-audio-out".into())
                .spawn(move || audio_playback_loop(rx, fmt, frame_us, enabled_bg, stop_bg, shared))
                .ok()?;
            Some(Self { tx, enabled, stop, join: Some(join) })
        }

        fn push(&self, pkt: EncodedAudio) {
            let _ = self.tx.try_send(pkt);
        }

        fn set_enabled(&self, on: bool) {
            self.enabled.store(on, Ordering::Relaxed);
        }

        fn stop(mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(j) = self.join.take() {
                let _ = j.join();
            }
        }
    }

    impl Drop for AudioPlaybackHandle {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(j) = self.join.take() {
                let _ = j.join();
            }
        }
    }

    fn audio_playback_loop(
        rx: Receiver<EncodedAudio>,
        fmt: AudioFormat,
        frame_us: u32,
        enabled: Arc<AtomicBool>,
        stop: Arc<AtomicBool>,
        shared: Arc<SessionShared>,
    ) {
        let frame_dur = Duration::from_micros(frame_us as u64);
        let mut decoder = match OpusDecoder::new(fmt, frame_dur) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("OpusDecoder init failed (audio disabled): {e}");
                return;
            }
        };
        let mut playback = match veloxdesk_audio::create_playback() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("audio playback unavailable: {e}");
                return;
            }
        };
        let target_buf = (frame_dur * 3).max(Duration::from_millis(15));
        if let Err(e) = playback.start(fmt, target_buf) {
            tracing::warn!("audio playback start failed: {e}");
            return;
        }
        tracing::info!(backend = playback.backend(), rate = fmt.sample_rate, ch = fmt.channels, frame_us, "audio playback ready");

        let mut jb = AudioJitterBuffer::new(JitterConfig {
            frame_duration: frame_dur,
            target_delay: (frame_dur * 3).max(Duration::from_millis(15)),
            max_delay: Duration::from_millis(80),
        });
        let mut next_tick = Instant::now() + frame_dur;
        while !stop.load(Ordering::Relaxed) {
            let now = Instant::now();
            if now < next_tick {
                match rx.recv_timeout(next_tick - now) {
                    Ok(pkt) => {
                        jb.push(pkt);
                        while let Ok(more) = rx.try_recv() {
                            jb.push(more);
                        }
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                }
            }
            let now = Instant::now();
            if now < next_tick {
                continue;
            }
            next_tick += frame_dur;
            if next_tick + frame_dur * 4 < now {
                next_tick = now + frame_dur;
            }
            match jb.pop_decoded(&mut decoder) {
                Ok(Some(pcm)) => {
                    if enabled.load(Ordering::Relaxed) {
                        let _ = playback.push(&pcm);
                    }
                }
                Ok(None) => {}
                Err(e) => tracing::debug!("audio decode error: {e}"),
            }
            let total_buf_us = playback.buffered_us().saturating_add(jb.stats().buffered_us);
            shared.audio_buf_us.store(total_buf_us, Ordering::Relaxed);
        }
        playback.stop();
        shared.audio_buf_us.store(0, Ordering::Relaxed);
    }

    struct MicCaptureHandle {
        enabled: Arc<AtomicBool>,
        stop: Arc<AtomicBool>,
        join: Option<std::thread::JoinHandle<()>>,
    }

    impl MicCaptureHandle {
        fn spawn(ch: Arc<dyn TransportChannel>, initial_enabled: bool) -> Self {
            let enabled = Arc::new(AtomicBool::new(initial_enabled));
            let stop = Arc::new(AtomicBool::new(false));
            let enabled_bg = enabled.clone();
            let stop_bg = stop.clone();
            let join = std::thread::Builder::new()
                .name("viewer-mic".into())
                .spawn(move || mic_capture_loop(ch, enabled_bg, stop_bg))
                .ok();
            Self { enabled, stop, join }
        }

        fn set_enabled(&self, on: bool) {
            self.enabled.store(on, Ordering::Relaxed);
        }

        fn stop(mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(j) = self.join.take() {
                let _ = j.join();
            }
        }
    }

    impl Drop for MicCaptureHandle {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(j) = self.join.take() {
                let _ = j.join();
            }
        }
    }

    fn mic_capture_loop(ch: Arc<dyn TransportChannel>, enabled: Arc<AtomicBool>, stop: Arc<AtomicBool>) {
        let mut sender = AudioSender::new(ch, 1);
        let mut cap: Option<Box<dyn veloxdesk_core_traits::AudioCapture>> = None;
        let mut enc: Option<OpusEncoder> = None;

        while !stop.load(Ordering::Relaxed) {
            if !enabled.load(Ordering::Relaxed) {
                if let Some(mut c) = cap.take() {
                    c.stop();
                }
                enc = None;
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            if cap.is_none() {
                let mut c = match veloxdesk_audio::create_mic_capture() {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!("mic capture unavailable: {e}");
                        enabled.store(false, Ordering::Relaxed);
                        continue;
                    }
                };
                let cfg = AudioCaptureConfig {
                    format: AudioFormat::STEREO_48K,
                    chunk: Duration::from_millis(5),
                    device: None,
                    microphone: true,
                };
                match c.start(&cfg) {
                    Ok(fmt) => {
                        let ecfg = AudioEncoderConfig {
                            format: fmt,
                            bitrate_kbps: 64,
                            frame_duration: Duration::from_millis(5),
                            low_delay: true,
                        };
                        match OpusEncoder::new(&ecfg) {
                            Ok(e) => {
                                tracing::info!(backend = c.backend(), ?fmt, "viewer microphone streaming enabled");
                                cap = Some(c);
                                enc = Some(e);
                            }
                            Err(e) => {
                                tracing::warn!("mic OpusEncoder failed: {e}");
                                c.stop();
                                enabled.store(false, Ordering::Relaxed);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("mic start failed: {e}");
                        enabled.store(false, Ordering::Relaxed);
                    }
                }
            }
            let (Some(c), Some(e)) = (cap.as_mut(), enc.as_mut()) else { continue };
            match c.read(Duration::from_millis(20)) {
                Ok(Some(pcm)) => {
                    if let Ok(pkts) = e.encode(&pcm) {
                        for pkt in pkts {
                            let _ = sender.send(&pkt);
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::debug!("mic read error: {e}");
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
        if let Some(mut c) = cap.take() {
            c.stop();
        }
    }

    fn build_hello(id: &DeviceIdentity, opts: &ConnectOptions) -> Hello {
        let mut video_caps = Vec::new();
        for cap in veloxdesk_codec::probe_decoders() {
            for codec in cap.codecs {
                let want_444 = cap.yuv444 && opts.yuv444;
                let want_10 = cap.ten_bit && opts.hdr_10bit;
                if let Some(existing) = video_caps.iter_mut().find(|c: &&mut VideoCodecCap| c.codec == codec as i32) {
                    existing.hw_decode |= cap.hardware;
                    existing.yuv444 |= want_444;
                    existing.ten_bit |= want_10;
                } else {
                    video_caps.push(VideoCodecCap {
                        codec: codec as i32,
                        hw_decode: cap.hardware,
                        max_width: 3840,
                        max_height: 2160,
                        max_fps: 240,
                        yuv444: want_444,
                        ten_bit: want_10,
                    });
                }
            }
        }
        let device_name = std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| "veloxdesk-viewer".into());
        Hello {
            protocol_version: PROTOCOL_VERSION,
            device_id: id.device_id(),
            public_key: id.public_key().to_vec(),
            device_name,
            os: std::env::consts::OS.to_string(),
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            mode: opts.mode as i32,
            video_caps,
            audio_codecs: if opts.audio_enabled { vec![AudioCodec::Opus as i32] } else { Vec::new() },
            viewer_width: opts.viewer_size.0,
            viewer_height: opts.viewer_size.1,
            viewer_refresh_hz: opts.viewer_refresh_hz,
            max_bitrate_kbps: opts.max_bitrate_kbps,
        }
    }

    /// Run one viewer session to completion. Returns `Some(reason)` when the session ends
    /// due to an error or remote close, or `None` when the local user disconnected.
    async fn run_viewer_session(
        opts: ConnectOptions,
        shared: Arc<SessionShared>,
        mut cmd_rx: mpsc::UnboundedReceiver<SessionCommand>,
        proxy: EventLoopProxy<UserEvent>,
    ) -> Option<String> {
        let id = match load_identity(None) {
            Ok(id) => id,
            Err(e) => return Some(tr(opts.lang()).fmt_err_identity(&format!("{e:#}"))),
        };

        let target_label = match &opts.target {
            ConnectTarget::Direct(addr) => addr.to_string(),
            ConnectTarget::DeviceId(target_id) => format_device_id(target_id),
        };
        let _ = proxy.send_event(UserEvent::Session(SessionEvent::Status(
            tr(opts.lang()).fmt_connecting_to(&target_label),
        )));

        // 1. QUIC connect (direct IP or P2P/relay by 9-digit Device ID; cancellable).
        let ch = match &opts.target {
            ConnectTarget::Direct(addr) => tokio::select! {
                res = timeout(CONNECT_TIMEOUT, veloxdesk_transport::connect(*addr, &id, None)) => {
                    match res {
                        Ok(Ok(ch)) => Arc::new(ch),
                        Ok(Err(e)) => return Some(tr(opts.lang()).fmt_err_connect_failed(&e.to_string())),
                        Err(_) => return Some(tr(opts.lang()).fmt_err_connect_timeout(&addr.to_string())),
                    }
                }
                () = recv_disconnect(&mut cmd_rx) => {
                    return None;
                }
            },
            ConnectTarget::DeviceId(target_id) => {
                let Some(rendezvous) = opts.rendezvous else {
                    return Some(tr(opts.lang()).err_no_rendezvous_configured.into());
                };
                let mut p2p_opts = veloxdesk_transport::P2pConnectOptions::new(rendezvous, target_id.clone());
                p2p_opts.relay_only = opts.relay_only;
                tokio::select! {
                    res = timeout(CONNECT_TIMEOUT, veloxdesk_transport::connect_p2p(p2p_opts, &id)) => {
                        match res {
                            Ok(Ok(ch)) => Arc::new(ch),
                            Ok(Err(e)) => return Some(tr(opts.lang()).fmt_err_p2p_connect_failed(&e.to_string())),
                            Err(_) => return Some(tr(opts.lang()).fmt_err_connect_timeout(&target_label)),
                        }
                    }
                    () = recv_disconnect(&mut cmd_rx) => {
                        return None;
                    }
                }
            }
        };

        let path = ch.stats().path;
        shared.hud.lock().snapshot.path = path;
        let _ = proxy.send_event(UserEvent::Session(SessionEvent::PathKind(path)));

        // 2. Open control stream and send Hello.
        let (send, recv) = match timeout(HANDSHAKE_TIMEOUT, ch.open_bi(StreamKind::Control)).await {
            Ok(Ok(pair)) => pair,
            Ok(Err(e)) => {
                ch.close(1, "control stream failed");
                return Some(tr(opts.lang()).fmt_err_open_control_stream(&e.to_string()));
            }
            Err(_) => {
                ch.close(1, "handshake timeout");
                return Some(tr(opts.lang()).err_open_control_timeout.into());
            }
        };
        let (mut ctrl_tx, mut ctrl_rx) = framed(send, recv);
        let hello = build_hello(&id, &opts);
        if let Err(e) = ctrl_tx.send(&Msg::Hello(hello).into()).await {
            if let Ok(Ok(Envelope { msg: Some(Msg::Close(c)) })) =
                timeout(Duration::from_millis(100), ctrl_rx.recv()).await
            {
                let reason = if c.reason.is_empty() {
                    tr(opts.lang()).fmt_host_rejected_code(c.code)
                } else {
                    c.reason
                };
                ch.close(0, "host closed");
                return Some(reason);
            }
            ch.close(1, "send hello failed");
            return Some(tr(opts.lang()).fmt_err_send_hello(&e.to_string()));
        }

        let _ = proxy.send_event(UserEvent::Session(SessionEvent::Status(
            tr(opts.lang()).waiting_for_host.into(),
        )));

        // 3. Handshake: optional AuthChallenge -> AuthResponse -> AuthResult -> SessionConfig.
        let initial_cfg = match handshake(
            &mut ctrl_tx,
            &mut ctrl_rx,
            &mut cmd_rx,
            &id,
            ch.peer_public_key(),
            opts.password.clone(),
            opts.lang(),
            &proxy,
        )
        .await
        {
            Ok(HandshakeOutcome::Config(cfg)) => cfg,
            Ok(HandshakeOutcome::Cancelled) => {
                ch.close(0, "cancelled");
                return None;
            }
            Err(e) => {
                ch.close(1, "handshake failed");
                return Some(format!("{e:#}"));
            }
        };

        // 4. Spawn control writer + dedicated input stream writer.
        let (ctrl_out_tx, mut ctrl_out_rx) = mpsc::unbounded_channel::<Envelope>();
        let ctrl_writer = tokio::spawn(async move {
            while let Some(env) = ctrl_out_rx.recv().await {
                if ctrl_tx.send(&env).await.is_err() {
                    break;
                }
            }
            let _ = ctrl_tx.finish().await;
        });

        let (input_tx, mut input_rx) = mpsc::unbounded_channel::<InputEvent>();
        let input_ch = ch.clone();
        let input_writer = tokio::spawn(async move {
            let Ok(stream) = input_ch.open_uni(StreamKind::Input).await else {
                tracing::warn!("could not open Input stream");
                return;
            };
            let mut sender = ControlSender::new(stream);
            while let Some(ev) = input_rx.recv().await {
                if sender.send(&Msg::Input(ev).into()).await.is_err() {
                    break;
                }
            }
            let _ = sender.finish().await;
        });

        // 5. Dedicated decoder thread (ADR-006).
        let decoder = match DecoderHandle::spawn(
            opts.prefer_hw_decode,
            opts.prefer_gpu_decode,
            shared.clone(),
            proxy.clone(),
        ) {
            Ok(d) => d,
            Err(e) => {
                ch.close(1, "decoder thread failed");
                return Some(format!("{e:#}"));
            }
        };

        // 5b. Audio playback thread, optional microphone thread, and physical gamepad poller (M3).
        let audio_out = AudioPlaybackHandle::spawn(initial_cfg.audio.as_ref(), opts.audio_enabled, shared.clone());
        let mic = MicCaptureHandle::spawn(ch.clone(), opts.mic_enabled);
        let (gp_tx, mut gp_rx) = mpsc::unbounded_channel::<GamepadState>();
        let gamepads = GamepadPoller::start(gp_tx, shared.active_gamepads.clone());
        let gp_ch = ch.clone();
        let gp_writer = tokio::spawn(async move {
            while let Some(state) = gp_rx.recv().await {
                if let Ok(dgram) = encode_gamepad_datagram(&state) {
                    let _ = gp_ch.send_datagram(dgram);
                }
            }
        });

        // 5c. M4 RustDesk-parity subsystems: Clipboard, File Transfer, TCP Tunnels, Remote Terminal.
        let mut clip_stop: Option<watch::Sender<bool>> = if opts.clipboard_sync {
            Some(start_clipboard_task(ch.clone()))
        } else {
            None
        };
        let ft_client = Arc::new(FileTransferClient::new(ch.clone()));
        let mut tunnels: HashMap<u64, LocalForwarder> = HashMap::new();
        let mut next_tunnel_id: u64 = 1;
        for spec in opts.initial_tunnels.clone() {
            add_tunnel(spec, ch.clone(), &mut tunnels, &mut next_tunnel_id, opts.lang(), &proxy).await;
        }
        let mut term_tx: Option<mpsc::UnboundedSender<TermCtrl>> = None;

        // 6. Initialize VideoReceiver + notify UI of initial SessionConfig.
        let mut receiver = VideoReceiver::new();
        if let Some(v) = &initial_cfg.video {
            receiver.set_dimensions(v.width, v.height);
            decoder.configure(v.clone());
        }
        let _ = proxy.send_event(UserEvent::Session(SessionEvent::Config(Box::new(initial_cfg))));

        // Immediate Ping #1 so ClockSync converges before the first HUD publish.
        let mut ping_seq: u32 = 1;
        let _ = ctrl_out_tx.send(Msg::Ping(Ping { seq: ping_seq, sent_us: Timestamp::now().as_micros() }).into());

        let mut feedback_tick = interval(FEEDBACK_TICK);
        feedback_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut ping_tick = interval(PING_TICK);
        ping_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Consume immediate first tick of ping_tick since Ping #1 was just sent.
        ping_tick.tick().await;

        // 7. Main session event loop.
        let close_reason = loop {
            tokio::select! {
                dgram = ch.recv_datagram() => {
                    match dgram {
                        Ok(buf) => {
                            let now = Timestamp::now();
                            match datagram_kind(&buf) {
                                Some(MediaKind::Audio) => {
                                    if let Some((stream_id, pkt)) = AudioReceiver::parse_datagram(&buf)
                                        && stream_id == 0
                                        && pkt.codec == AudioCodecId::Opus
                                        && let Some(ao) = &audio_out
                                    {
                                        ao.push(pkt);
                                    }
                                }
                                Some(MediaKind::Video) | None => {
                                    shared.hud.lock().on_bytes_received(buf.len());
                                    if let Some(rf) = receiver.push_datagram(&buf, now)
                                        && !decoder.push_frame(rf)
                                    {
                                        receiver.request_keyframe();
                                    }
                                    while let Some(rf) = receiver.pop_frame() {
                                        if !decoder.push_frame(rf) {
                                            receiver.request_keyframe();
                                        }
                                    }
                                    let fb = receiver.poll_feedback(now);
                                    send_feedback(fb, &ctrl_out_tx, &shared);
                                }
                                Some(_) => {}
                            }
                        }
                        Err(CoreError::Closed) => break Some("host closed connection".into()),
                        Err(e) => break Some(format!("transport error: {e}")),
                    }
                }
                msg = ctrl_rx.recv() => {
                    match msg {
                        Ok(env) => match env.msg {
                            Some(Msg::Pong(p)) => {
                                let t3 = Timestamp::now().as_micros();
                                let rtt = {
                                    let mut clock = shared.clock.lock();
                                    clock.add_sample(p.ping_sent_us, p.peer_recv_us, p.peer_send_us, t3);
                                    clock.estimate().map(|(rtt, _)| rtt as u32)
                                };
                                receiver.set_host_time_reference(Timestamp(p.peer_send_us));
                                if let Some(rtt_us) = rtt {
                                    receiver.set_rtt_us(rtt_us);
                                    shared.hud.lock().on_rtt(rtt_us);
                                }
                            }
                            Some(Msg::FrameTiming(ft)) => {
                                receiver.set_host_time_reference(Timestamp(ft.capture_ts_us));
                                shared.hud.lock().on_frame_timing(ft);
                            }
                            Some(Msg::SessionConfig(cfg)) => {
                                if let Some(v) = &cfg.video {
                                    receiver.set_dimensions(v.width, v.height);
                                    decoder.configure(v.clone());
                                }
                                let _ = proxy.send_event(UserEvent::Session(SessionEvent::Config(Box::new(cfg))));
                            }
                            Some(Msg::VideoConfig(v)) => {
                                receiver.set_dimensions(v.width, v.height);
                                decoder.configure(v);
                            }
                            Some(Msg::CursorShape(cs)) => {
                                let img = CursorImage {
                                    id: cs.id,
                                    width: cs.width,
                                    height: cs.height,
                                    hotspot_x: cs.hotspot_x,
                                    hotspot_y: cs.hotspot_y,
                                    rgba: cs.rgba,
                                };
                                let _ = proxy.send_event(UserEvent::Session(SessionEvent::CursorShape(img)));
                            }
                            Some(Msg::CursorPosition(cp)) => {
                                let _ = proxy.send_event(UserEvent::Session(SessionEvent::CursorPos {
                                    x: cp.x,
                                    y: cp.y,
                                    visible: cp.visible,
                                }));
                            }
                            Some(Msg::DisplayList(dl)) => {
                                let _ = proxy.send_event(UserEvent::Session(SessionEvent::DisplayList(dl.displays)));
                            }
                            Some(Msg::PrivacyModeResult(r)) => {
                                let err = (!r.error.is_empty()).then_some(r.error);
                                let _ = proxy.send_event(UserEvent::Session(SessionEvent::PrivacyMode {
                                    enabled: r.enabled,
                                    error: err,
                                }));
                            }
                            Some(Msg::Rumble(r)) => {
                                GamepadPoller::apply_rumble(&r);
                            }
                            Some(Msg::BitrateUpdate(b)) => {
                                tracing::debug!(
                                    bitrate_kbps = b.bitrate_kbps,
                                    fps = b.fps,
                                    fec = b.fec_parity_percent,
                                    "host bitrate update"
                                );
                            }
                            Some(Msg::Close(c)) => {
                                let reason = if c.reason.is_empty() {
                                    format!("host closed session (code {})", c.code)
                                } else {
                                    c.reason
                                };
                                break Some(reason);
                            }
                            Some(other) => tracing::trace!(?other, "ignored control message"),
                            None => {}
                        },
                        Err(CoreError::Closed) => break Some("host closed control stream".into()),
                        Err(e) => break Some(format!("control stream error: {e}")),
                    }
                }
                cmd = cmd_rx.recv() => {
                    match cmd {
                        Some(SessionCommand::Input(ev)) => {
                            let _ = input_tx.send(ev);
                        }
                        Some(SessionCommand::SwitchDisplay(index)) => {
                            let _ = ctrl_out_tx.send(Msg::SwitchDisplay(SwitchDisplay { index }).into());
                        }
                        Some(SessionCommand::SetMaxBitrate(kbps)) => {
                            let _ = ctrl_out_tx.send(
                                Msg::BitrateUpdate(BitrateUpdate {
                                    stream_id: 0,
                                    bitrate_kbps: kbps,
                                    fps: 0,
                                    fec_parity_percent: 0,
                                })
                                .into(),
                            );
                        }
                        Some(SessionCommand::SetAudio(on)) => {
                            if let Some(ao) = &audio_out {
                                ao.set_enabled(on);
                            }
                        }
                        Some(SessionCommand::SetMic(on)) => {
                            mic.set_enabled(on);
                        }
                        Some(SessionCommand::SetClipboardSync(on)) => {
                            if on {
                                if clip_stop.is_none() {
                                    clip_stop = Some(start_clipboard_task(ch.clone()));
                                }
                            } else if let Some(stop) = clip_stop.take() {
                                let _ = stop.send(true);
                            }
                        }
                        Some(SessionCommand::SetPrivacyMode(enable)) => {
                            let _ = ctrl_out_tx.send(Msg::PrivacyMode(PrivacyMode { enable }).into());
                        }
                        Some(SessionCommand::File(action)) => {
                            spawn_file_action(ft_client.clone(), action, opts.lang(), proxy.clone());
                        }
                        Some(SessionCommand::Terminal(action)) => match action {
                            TerminalAction::Open { cols, rows, shell } => {
                                if let Some(tx) = term_tx.take() {
                                    let _ = tx.send(TermCtrl::Close);
                                }
                                term_tx = Some(spawn_terminal_task(ch.clone(), cols, rows, shell, proxy.clone()));
                            }
                            TerminalAction::SendInput(data) => {
                                if let Some(tx) = &term_tx {
                                    let _ = tx.send(TermCtrl::Input(data));
                                }
                            }
                            TerminalAction::Resize { cols, rows } => {
                                if let Some(tx) = &term_tx {
                                    let _ = tx.send(TermCtrl::Resize { cols, rows });
                                }
                            }
                            TerminalAction::Close => {
                                if let Some(tx) = term_tx.take() {
                                    let _ = tx.send(TermCtrl::Close);
                                }
                            }
                        },
                        Some(SessionCommand::Tunnel(action)) => match action {
                            TunnelAction::Add(spec) => {
                                add_tunnel(spec, ch.clone(), &mut tunnels, &mut next_tunnel_id, opts.lang(), &proxy)
                                    .await;
                            }
                            TunnelAction::Remove(id) => {
                                if let Some(fwd) = tunnels.remove(&id) {
                                    fwd.shutdown();
                                    let _ = proxy.send_event(UserEvent::Session(SessionEvent::TunnelRemoved(id)));
                                }
                            }
                        },
                        Some(SessionCommand::SubmitPassword(_)) | Some(SessionCommand::CancelAuth) => {}
                        Some(SessionCommand::Disconnect) | None => {
                            let _ = ctrl_out_tx.send(
                                Msg::Close(Close { code: 0, reason: "viewer disconnected".into() }).into(),
                            );
                            break None;
                        }
                    }
                }
                _ = feedback_tick.tick() => {
                    if decoder.take_pli_request() {
                        receiver.request_keyframe();
                    }
                    receiver.set_decode_render_us(
                        shared.decode_us.load(Ordering::Relaxed),
                        shared.render_us.load(Ordering::Relaxed),
                    );
                    let stats = ch.stats();
                    if stats.rtt_us > 0 && shared.clock.lock().estimate().is_none() {
                        receiver.set_rtt_us(stats.rtt_us);
                        shared.hud.lock().on_rtt(stats.rtt_us);
                    }
                    let now = Timestamp::now();
                    let fb = receiver.poll_feedback(now);
                    send_feedback(fb, &ctrl_out_tx, &shared);
                    while let Some(rf) = receiver.pop_frame() {
                        if !decoder.push_frame(rf) {
                            receiver.request_keyframe();
                        }
                    }
                }
                _ = ping_tick.tick() => {
                    ping_seq = ping_seq.wrapping_add(1);
                    let _ = ctrl_out_tx.send(
                        Msg::Ping(Ping { seq: ping_seq, sent_us: Timestamp::now().as_micros() }).into(),
                    );
                }
            }
        };

        // 8. Clean teardown.
        if let Some(stop) = clip_stop.take() {
            let _ = stop.send(true);
        }
        if let Some(tx) = term_tx.take() {
            let _ = tx.send(TermCtrl::Close);
        }
        for (_, fwd) in tunnels.drain() {
            fwd.shutdown();
        }
        gamepads.stop();
        mic.stop();
        if let Some(ao) = audio_out {
            ao.stop();
        }
        drop(input_tx);
        drop(ctrl_out_tx);
        let _ = timeout(Duration::from_millis(200), ctrl_writer).await;
        let _ = timeout(Duration::from_millis(200), input_writer).await;
        let _ = timeout(Duration::from_millis(200), gp_writer).await;
        ch.close(0, "session ended");
        // Briefly yield so quinn flushes the CONNECTION_CLOSE UDP packet before runtime teardown.
        tokio::time::sleep(Duration::from_millis(15)).await;
        decoder.stop();
        close_reason
    }

    enum HandshakeOutcome {
        Config(SessionConfig),
        Cancelled,
    }

    async fn recv_disconnect(cmd_rx: &mut mpsc::UnboundedReceiver<SessionCommand>) {
        while let Some(cmd) = cmd_rx.recv().await {
            if matches!(cmd, SessionCommand::Disconnect | SessionCommand::CancelAuth) {
                return;
            }
        }
    }

    async fn wait_for_password(cmd_rx: &mut mpsc::UnboundedReceiver<SessionCommand>) -> Option<String> {
        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                SessionCommand::SubmitPassword(pw) => return Some(pw),
                SessionCommand::CancelAuth | SessionCommand::Disconnect => return None,
                _ => {}
            }
        }
        None
    }

    #[allow(clippy::too_many_arguments)]
    async fn respond_to_challenge(
        challenge: &AuthChallenge,
        id: &DeviceIdentity,
        tls_peer_pk: Option<[u8; 32]>,
        cli_password: &mut Option<String>,
        cmd_rx: &mut mpsc::UnboundedReceiver<SessionCommand>,
        lang: Language,
        proxy: &EventLoopProxy<UserEvent>,
    ) -> anyhow::Result<Option<AuthResponse>> {
        let t = tr(lang);
        let host_pk: [u8; 32] = if challenge.host_public_key.is_empty() {
            tls_peer_pk.unwrap_or([0u8; 32])
        } else {
            challenge
                .host_public_key
                .as_slice()
                .try_into()
                .map_err(|_| anyhow::anyhow!("{}", t.err_auth_pk_len))?
        };
        if let Some(tls_pk) = tls_peer_pk
            && !challenge.host_public_key.is_empty()
            && tls_pk != host_pk
        {
            bail!("{}", t.err_auth_pk_mismatch);
        }
        if !challenge.host_device_id.is_empty() {
            let derived = veloxdesk_crypto::device_id_from_public_key(&host_pk);
            if derived != challenge.host_device_id {
                bail!("{}", t.err_auth_device_id_mismatch);
            }
        }

        let has = |m: AuthMethod| challenge.methods.contains(&(m as i32));

        // 1. Trusted Ed25519 public key (passwordless).
        if has(AuthMethod::PublicKey) {
            let sig = sign_pubkey_challenge(id, &challenge.nonce, &host_pk);
            return Ok(Some(AuthResponse {
                method: AuthMethod::PublicKey as i32,
                proof: sig.to_vec(),
            }));
        }

        // 2. Password / OTP challenge (Argon2id + HMAC-SHA256; never sent in clear).
        let allow_perm = has(AuthMethod::PermanentPassword);
        let allow_otp = has(AuthMethod::OneTimePassword);
        if allow_perm || allow_otp {
            let password = match cli_password.take() {
                Some(pw) => pw,
                None => {
                    let methods_hint = match (allow_perm, allow_otp) {
                        (true, true) => t.auth_methods_both.into(),
                        (true, false) => t.auth_methods_perm.into(),
                        (false, true) => t.auth_methods_otp.into(),
                        (false, false) => unreachable!(),
                    };
                    let host_id = if challenge.host_device_id.is_empty() {
                        veloxdesk_crypto::device_id_from_public_key(&host_pk)
                    } else {
                        challenge.host_device_id.clone()
                    };
                    let _ = proxy.send_event(UserEvent::Session(SessionEvent::PasswordPrompt {
                        host_device_id: host_id,
                        host_fingerprint: veloxdesk_crypto::fingerprint(&host_pk),
                        methods_hint,
                        error: None,
                    }));
                    let Some(pw) = wait_for_password(cmd_rx).await else {
                        return Ok(None);
                    };
                    pw
                }
            };

            let _ = proxy.send_event(UserEvent::Session(SessionEvent::Status(t.authenticating.into())));
            let method = if allow_perm && !challenge.salt.is_empty() {
                AuthMethod::PermanentPassword
            } else if allow_otp {
                AuthMethod::OneTimePassword
            } else {
                AuthMethod::PermanentPassword
            };

            let params = if challenge.argon2_m_cost_kib > 0 && challenge.argon2_t_cost > 0 && challenge.argon2_p_cost > 0 {
                Argon2Params {
                    m_cost_kib: challenge.argon2_m_cost_kib,
                    t_cost: challenge.argon2_t_cost,
                    p_cost: challenge.argon2_p_cost,
                }
            } else {
                Argon2Params::default()
            };
            let salt = challenge.salt.clone();
            let nonce = challenge.nonce.clone();
            let proof = tokio::task::spawn_blocking(move || -> anyhow::Result<[u8; 32]> {
                let key = derive_password_key(&password, &salt, params)
                    .map_err(|e| anyhow::anyhow!("{}", tr(lang).fmt_err_kdf(&e.to_string())))?;
                Ok(password_proof(&key, &nonce))
            })
            .await
            .context(t.err_kdf_panicked)??;

            return Ok(Some(AuthResponse { method: method as i32, proof: proof.to_vec() }));
        }

        // 3. Interactive approval only (no password required).
        if has(AuthMethod::None) || challenge.methods.is_empty() {
            let _ = proxy.send_event(UserEvent::Session(SessionEvent::Status(
                t.waiting_for_host_accept.into(),
            )));
            return Ok(Some(AuthResponse { method: AuthMethod::None as i32, proof: Vec::new() }));
        }

        bail!("{}", t.fmt_err_no_auth_methods(&challenge.methods));
    }

    #[allow(clippy::too_many_arguments)]
    async fn handshake(
        ctrl_tx: &mut ControlSender,
        ctrl_rx: &mut veloxdesk_transport::ControlReceiver,
        cmd_rx: &mut mpsc::UnboundedReceiver<SessionCommand>,
        id: &DeviceIdentity,
        tls_peer_pk: Option<[u8; 32]>,
        mut cli_password: Option<String>,
        lang: Language,
        proxy: &EventLoopProxy<UserEvent>,
    ) -> anyhow::Result<HandshakeOutcome> {
        let t = tr(lang);
        let mut step_timeout = HANDSHAKE_TIMEOUT;
        loop {
            let env = tokio::select! {
                res = timeout(step_timeout, ctrl_rx.recv()) => {
                    res.context(t.err_handshake_timeout)?.context(t.err_reading_handshake)?
                }
                () = recv_disconnect(cmd_rx) => return Ok(HandshakeOutcome::Cancelled),
            };
            match env.msg {
                Some(Msg::AuthChallenge(challenge)) => {
                    let Some(resp) = respond_to_challenge(
                        &challenge,
                        id,
                        tls_peer_pk,
                        &mut cli_password,
                        cmd_rx,
                        lang,
                        proxy,
                    )
                    .await?
                    else {
                        return Ok(HandshakeOutcome::Cancelled);
                    };
                    ctrl_tx.send(&Msg::AuthResponse(resp).into()).await.context(t.err_sending_auth_response)?;
                    // After responding, host may present an interactive Accept dialog.
                    step_timeout = APPROVAL_TIMEOUT;
                }
                Some(Msg::AuthResult(r)) => {
                    if !r.ok {
                        let reason = if r.reason.is_empty() { t.err_auth_rejected.into() } else { r.reason };
                        bail!("{reason}");
                    }
                    tracing::info!(session_id = r.session_id, "authenticated with host");
                    step_timeout = HANDSHAKE_TIMEOUT;
                }
                Some(Msg::SessionConfig(cfg)) => {
                    return Ok(HandshakeOutcome::Config(cfg));
                }
                Some(Msg::Close(c)) => {
                    let reason = if c.reason.is_empty() {
                        t.fmt_host_rejected_code(c.code)
                    } else {
                        c.reason
                    };
                    bail!("{reason}");
                }
                Some(other) => tracing::debug!(?other, "message during handshake ignored"),
                None => {}
            }
        }
    }

    fn send_feedback(
        envelopes: Vec<Envelope>,
        ctrl_out: &mpsc::UnboundedSender<Envelope>,
        shared: &SessionShared,
    ) {
        for env in envelopes {
            if let Some(Msg::ReceiverReport(rr)) = &env.msg {
                shared.hud.lock().on_receiver_report(rr);
            }
            let _ = ctrl_out.send(env);
        }
    }

    // ───────────────────────── M4 feature helpers ─────────────────────────

    fn start_clipboard_task(ch: Arc<dyn TransportChannel>) -> watch::Sender<bool> {
        let (stop_tx, stop_rx) = watch::channel(false);
        tokio::spawn(async move {
            let backend = match veloxdesk_clipboard::create_system_clipboard() {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!("system clipboard unavailable: {e}");
                    return;
                }
            };
            match ch.open_bi(StreamKind::Clipboard).await {
                Ok((send, recv)) => {
                    if let Err(e) = veloxdesk_clipboard::run_clipboard_stream(
                        send,
                        recv,
                        backend,
                        Duration::from_millis(250),
                        stop_rx,
                    )
                    .await
                    {
                        tracing::debug!("clipboard sync ended: {e}");
                    }
                }
                Err(e) => tracing::warn!("open Clipboard stream failed: {e}"),
            }
        });
        stop_tx
    }

    async fn add_tunnel(
        spec: TunnelSpec,
        ch: Arc<dyn TransportChannel>,
        tunnels: &mut HashMap<u64, LocalForwarder>,
        next_id: &mut u64,
        lang: Language,
        proxy: &EventLoopProxy<UserEvent>,
    ) {
        let bind_addr = SocketAddr::from(([127, 0, 0, 1], spec.local_port));
        match LocalForwarder::bind(bind_addr, spec.remote_host.clone(), spec.remote_port, ch).await {
            Ok(fwd) => {
                let id = *next_id;
                *next_id += 1;
                let local_addr = fwd.local_addr().to_string();
                tracing::info!(id, %local_addr, remote = %format!("{}:{}", spec.remote_host, spec.remote_port), "TCP tunnel listening");
                tunnels.insert(id, fwd);
                let _ = proxy.send_event(UserEvent::Session(SessionEvent::TunnelAdded(ActiveTunnel {
                    id,
                    local_addr,
                    remote_host: spec.remote_host,
                    remote_port: spec.remote_port,
                })));
            }
            Err(e) => {
                let _ = proxy.send_event(UserEvent::Session(SessionEvent::TunnelError(
                    tr(lang).fmt_err_tunnel(spec.local_port, &spec.remote_host, spec.remote_port, &e.to_string()),
                )));
            }
        }
    }

    fn spawn_file_action(
        client: Arc<FileTransferClient>,
        action: FileAction,
        lang: Language,
        proxy: EventLoopProxy<UserEvent>,
    ) {
        tokio::spawn(async move {
            let t = tr(lang);
            match action {
                FileAction::ListDir(path) => match client.list_dir(&path).await {
                    Ok(entries) => {
                        let _ = proxy.send_event(UserEvent::Session(SessionEvent::FileListing { path, entries }));
                    }
                    Err(e) => {
                        let _ = proxy.send_event(UserEvent::Session(SessionEvent::FileError(e.to_string())));
                    }
                },
                FileAction::Mkdir(path) => match client.mkdir(&path).await {
                    Ok(()) => {
                        let _ = proxy.send_event(UserEvent::Session(SessionEvent::FileStatus(
                            t.fmt_file_created_dir(&path),
                        )));
                        let parent = veloxdesk_ui::parent_remote_path(&path);
                        if let Ok(entries) = client.list_dir(&parent).await {
                            let _ = proxy.send_event(UserEvent::Session(SessionEvent::FileListing {
                                path: parent,
                                entries,
                            }));
                        }
                    }
                    Err(e) => {
                        let _ = proxy.send_event(UserEvent::Session(SessionEvent::FileError(e.to_string())));
                    }
                },
                FileAction::Delete(path) => match client.delete(&path).await {
                    Ok(()) => {
                        let _ = proxy.send_event(UserEvent::Session(SessionEvent::FileStatus(
                            t.fmt_file_deleted(&path),
                        )));
                        let parent = veloxdesk_ui::parent_remote_path(&path);
                        if let Ok(entries) = client.list_dir(&parent).await {
                            let _ = proxy.send_event(UserEvent::Session(SessionEvent::FileListing {
                                path: parent,
                                entries,
                            }));
                        }
                    }
                    Err(e) => {
                        let _ = proxy.send_event(UserEvent::Session(SessionEvent::FileError(e.to_string())));
                    }
                },
                FileAction::Rename { from, to } => match client.rename(&from, &to).await {
                    Ok(()) => {
                        let _ = proxy.send_event(UserEvent::Session(SessionEvent::FileStatus(
                            t.fmt_file_renamed(&to),
                        )));
                        let parent = veloxdesk_ui::parent_remote_path(&to);
                        if let Ok(entries) = client.list_dir(&parent).await {
                            let _ = proxy.send_event(UserEvent::Session(SessionEvent::FileListing {
                                path: parent,
                                entries,
                            }));
                        }
                    }
                    Err(e) => {
                        let _ = proxy.send_event(UserEvent::Session(SessionEvent::FileError(e.to_string())));
                    }
                },
                FileAction::Download { remote_path, local_path, resume, compress } => {
                    let label = t.fmt_file_downloading(&remote_path);
                    let p_proxy = proxy.clone();
                    let p_label = label.clone();
                    let cb = Arc::new(move |done: u64, total: u64| {
                        let _ = p_proxy.send_event(UserEvent::Session(SessionEvent::FileProgress(Some(
                            TransferProgress { label: p_label.clone(), bytes_done: done, bytes_total: total },
                        ))));
                    });
                    match client
                        .download(&remote_path, Path::new(&local_path), resume, compress, Some(cb))
                        .await
                    {
                        Ok(rep) => {
                            let _ = proxy.send_event(UserEvent::Session(SessionEvent::FileStatus(
                                t.fmt_file_downloaded(&remote_path, rep.total_bytes, rep.resumed_from),
                            )));
                        }
                        Err(e) => {
                            let _ = proxy.send_event(UserEvent::Session(SessionEvent::FileError(e.to_string())));
                        }
                    }
                }
                FileAction::Upload { local_path, remote_path, resume, compress } => {
                    let label = t.fmt_file_uploading(&remote_path);
                    let p_proxy = proxy.clone();
                    let p_label = label.clone();
                    let cb = Arc::new(move |done: u64, total: u64| {
                        let _ = p_proxy.send_event(UserEvent::Session(SessionEvent::FileProgress(Some(
                            TransferProgress { label: p_label.clone(), bytes_done: done, bytes_total: total },
                        ))));
                    });
                    match client
                        .upload(Path::new(&local_path), &remote_path, resume, compress, Some(cb))
                        .await
                    {
                        Ok(rep) => {
                            let _ = proxy.send_event(UserEvent::Session(SessionEvent::FileStatus(
                                t.fmt_file_uploaded(&remote_path, rep.total_bytes, rep.resumed_from),
                            )));
                            let parent = veloxdesk_ui::parent_remote_path(&remote_path);
                            if let Ok(entries) = client.list_dir(&parent).await {
                                let _ = proxy.send_event(UserEvent::Session(SessionEvent::FileListing {
                                    path: parent,
                                    entries,
                                }));
                            }
                        }
                        Err(e) => {
                            let _ = proxy.send_event(UserEvent::Session(SessionEvent::FileError(e.to_string())));
                        }
                    }
                }
            }
        });
    }

    enum TermCtrl {
        Input(Vec<u8>),
        Resize { cols: u32, rows: u32 },
        Close,
    }

    fn spawn_terminal_task(
        ch: Arc<dyn TransportChannel>,
        cols: u32,
        rows: u32,
        shell: String,
        proxy: EventLoopProxy<UserEvent>,
    ) -> mpsc::UnboundedSender<TermCtrl> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut client = match TerminalClient::open(ch.as_ref(), cols, rows, shell).await {
                Ok(c) => c,
                Err(e) => {
                    let _ = proxy.send_event(UserEvent::Session(SessionEvent::TerminalError(e.to_string())));
                    return;
                }
            };
            let _ = proxy.send_event(UserEvent::Session(SessionEvent::TerminalOpened));
            loop {
                tokio::select! {
                    ev = client.recv() => match ev {
                        Ok(TerminalEvent::Data(bytes)) => {
                            let _ = proxy.send_event(UserEvent::Session(SessionEvent::TerminalOutput(bytes.to_vec())));
                        }
                        Ok(TerminalEvent::Exit(code)) => {
                            let _ = proxy.send_event(UserEvent::Session(SessionEvent::TerminalExit(code)));
                            break;
                        }
                        Err(CoreError::Closed) => {
                            let _ = proxy.send_event(UserEvent::Session(SessionEvent::TerminalExit(0)));
                            break;
                        }
                        Err(e) => {
                            let _ = proxy.send_event(UserEvent::Session(SessionEvent::TerminalError(e.to_string())));
                            break;
                        }
                    },
                    cmd = rx.recv() => match cmd {
                        Some(TermCtrl::Input(data)) => {
                            if let Err(e) = client.write(&data).await {
                                let _ = proxy.send_event(UserEvent::Session(SessionEvent::TerminalError(e.to_string())));
                                break;
                            }
                        }
                        Some(TermCtrl::Resize { cols, rows }) => {
                            let _ = client.resize(cols, rows).await;
                        }
                        Some(TermCtrl::Close) | None => break,
                    },
                }
            }
        });
        tx
    }
}
