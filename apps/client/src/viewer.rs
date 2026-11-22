//! Viewer network + decode session runner (ADR-006: tokio for QUIC/control, dedicated OS
//! thread for video decoding, "latest frame wins" handoff to the winit/wgpu render thread).

#[cfg(feature = "net")]
pub use net::{connect, device_id_label, load_identity};

#[cfg(not(feature = "net"))]
pub fn device_id_label() -> String {
    String::new()
}

#[cfg(not(feature = "net"))]
pub fn connect(
    _opts: crate::session::ConnectOptions,
    _proxy: veloxdesk_render::winit::event_loop::EventLoopProxy<crate::app::UserEvent>,
) -> anyhow::Result<crate::session::SessionHandle> {
    anyhow::bail!("network support disabled at compile time (rebuild with `--features net`)")
}

#[cfg(feature = "net")]
mod net {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, OnceLock};
    use std::time::Duration;

    use anyhow::{Context, bail};
    use crossbeam_channel::{Receiver, Sender, TrySendError};
    use tokio::sync::mpsc;
    use tokio::time::{interval, timeout};
    use veloxdesk_core_traits::video::{CursorImage, DecoderConfig, VideoCodec, VideoDecoder};
    use veloxdesk_core_traits::{CoreError, Timestamp, TransportChannel};
    use veloxdesk_crypto::DeviceIdentity;
    use veloxdesk_crypto::auth::{Argon2Params, derive_password_key, password_proof, sign_pubkey_challenge};
    use veloxdesk_protocol::control::{
        AuthChallenge, AuthMethod, AuthResponse, BitrateUpdate, Close, Envelope, Hello, InputEvent, Ping,
        SessionConfig, SwitchDisplay, VideoCodecCap, VideoConfig, envelope::Msg,
    };
    use veloxdesk_protocol::{PROTOCOL_VERSION, StreamKind};
    use veloxdesk_render::winit::event_loop::EventLoopProxy;
    use veloxdesk_transport::{ControlSender, ReceivedFrame, VideoReceiver, framed};
    use veloxdesk_ui::{ConnectTarget, format_device_id};

    use crate::app::UserEvent;
    use crate::session::{ConnectOptions, DecodedMsg, SessionCommand, SessionEvent, SessionHandle, SessionShared};

    const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
    const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
    /// Generous wait for an interactive host Accept dialog after `AuthResponse`.
    const APPROVAL_TIMEOUT: Duration = Duration::from_secs(60);
    const FEEDBACK_TICK: Duration = Duration::from_millis(5);
    const PING_TICK: Duration = Duration::from_millis(500);
    const DECODER_QUEUE_DEPTH: usize = 4;

    /// Default path for the persisted Ed25519 identity key.
    fn default_identity_path() -> Option<PathBuf> {
        veloxdesk_ui::settings::config_dir().map(|d| d.join("veloxdesk").join("identity.key"))
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
                        let _ = proxy.send_event(UserEvent::Session(SessionEvent::Closed(format!(
                            "cannot start async runtime: {e}"
                        ))));
                        return;
                    }
                };
                let closed_reason = rt.block_on(run_viewer_session(opts, shared_bg, cmd_rx, proxy.clone()));
                if let Some(reason) = closed_reason {
                    let _ = proxy.send_event(UserEvent::Session(SessionEvent::Closed(reason)));
                }
            })
            .context("spawning viewer network thread")?;
        Ok(SessionHandle { shared, cmd: cmd_tx })
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
                .spawn(move || decoder_loop(rx, prefer_hw, shared, proxy, need_kf, pli))
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
                    match open_decoder(codec, vcfg.width, vcfg.height, prefer_hw, &proxy) {
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
                        match open_decoder(rf.packet.codec, rf.packet.width, rf.packet.height, prefer_hw, &proxy) {
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
        proxy: &EventLoopProxy<UserEvent>,
    ) -> veloxdesk_core_traits::Result<Box<dyn VideoDecoder>> {
        // M1 uses CPU NV12 upload (`veloxdesk-render` zero-copy import is M3).
        let cfg = DecoderConfig { codec, width, height, prefer_gpu_output: false, shared_device: None };
        let dec = veloxdesk_codec::create_decoder(&cfg, prefer_hw)?;
        let caps = dec.caps();
        tracing::info!(
            decoder = caps.name,
            hardware = caps.hardware,
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

    fn build_hello(id: &DeviceIdentity, opts: &ConnectOptions) -> Hello {
        let mut video_caps = Vec::new();
        for cap in veloxdesk_codec::probe_decoders() {
            for codec in cap.codecs {
                if let Some(existing) = video_caps.iter_mut().find(|c: &&mut VideoCodecCap| c.codec == codec as i32) {
                    existing.hw_decode |= cap.hardware;
                    existing.yuv444 |= cap.yuv444;
                    existing.ten_bit |= cap.ten_bit;
                } else {
                    video_caps.push(VideoCodecCap {
                        codec: codec as i32,
                        hw_decode: cap.hardware,
                        max_width: 3840,
                        max_height: 2160,
                        max_fps: 240,
                        yuv444: cap.yuv444,
                        ten_bit: cap.ten_bit,
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
            audio_codecs: Vec::new(),
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
            Err(e) => return Some(format!("identity error: {e:#}")),
        };

        let target_label = match &opts.target {
            ConnectTarget::Direct(addr) => addr.to_string(),
            ConnectTarget::DeviceId(target_id) => format_device_id(target_id),
        };
        let _ = proxy.send_event(UserEvent::Session(SessionEvent::Status(format!(
            "Connecting to {target_label}…"
        ))));

        // 1. QUIC connect (direct IP or P2P/relay by 9-digit Device ID; cancellable).
        let ch = match &opts.target {
            ConnectTarget::Direct(addr) => tokio::select! {
                res = timeout(CONNECT_TIMEOUT, veloxdesk_transport::connect(*addr, &id, None)) => {
                    match res {
                        Ok(Ok(ch)) => Arc::new(ch),
                        Ok(Err(e)) => return Some(format!("connect failed: {e}")),
                        Err(_) => return Some(format!("timed out connecting to {addr}")),
                    }
                }
                () = recv_disconnect(&mut cmd_rx) => {
                    return None;
                }
            },
            ConnectTarget::DeviceId(target_id) => {
                let Some(rendezvous) = opts.rendezvous else {
                    return Some("no rendezvous server configured".into());
                };
                let mut p2p_opts = veloxdesk_transport::P2pConnectOptions::new(rendezvous, target_id.clone());
                p2p_opts.relay_only = opts.relay_only;
                tokio::select! {
                    res = timeout(CONNECT_TIMEOUT, veloxdesk_transport::connect_p2p(p2p_opts, &id)) => {
                        match res {
                            Ok(Ok(ch)) => Arc::new(ch),
                            Ok(Err(e)) => return Some(format!("P2P connect failed: {e}")),
                            Err(_) => return Some(format!("timed out connecting to {target_label}")),
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
                return Some(format!("open control stream failed: {e}"));
            }
            Err(_) => {
                ch.close(1, "handshake timeout");
                return Some("timed out opening control stream".into());
            }
        };
        let (mut ctrl_tx, mut ctrl_rx) = framed(send, recv);
        let hello = build_hello(&id, &opts);
        if let Err(e) = ctrl_tx.send(&Msg::Hello(hello).into()).await {
            ch.close(1, "send hello failed");
            return Some(format!("send Hello failed: {e}"));
        }

        let _ = proxy.send_event(UserEvent::Session(SessionEvent::Status("Waiting for host…".into())));

        // 3. Handshake: optional AuthChallenge -> AuthResponse -> AuthResult -> SessionConfig.
        let initial_cfg = match handshake(
            &mut ctrl_tx,
            &mut ctrl_rx,
            &mut cmd_rx,
            &id,
            ch.peer_public_key(),
            opts.password.clone(),
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
        let decoder = match DecoderHandle::spawn(opts.prefer_hw_decode, shared.clone(), proxy.clone()) {
            Ok(d) => d,
            Err(e) => {
                ch.close(1, "decoder thread failed");
                return Some(format!("{e:#}"));
            }
        };

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
        drop(input_tx);
        drop(ctrl_out_tx);
        let _ = timeout(Duration::from_millis(200), ctrl_writer).await;
        let _ = timeout(Duration::from_millis(200), input_writer).await;
        ch.close(0, "session ended");
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

    async fn respond_to_challenge(
        challenge: &AuthChallenge,
        id: &DeviceIdentity,
        tls_peer_pk: Option<[u8; 32]>,
        cli_password: &mut Option<String>,
        cmd_rx: &mut mpsc::UnboundedReceiver<SessionCommand>,
        proxy: &EventLoopProxy<UserEvent>,
    ) -> anyhow::Result<Option<AuthResponse>> {
        let host_pk: [u8; 32] = if challenge.host_public_key.is_empty() {
            tls_peer_pk.unwrap_or([0u8; 32])
        } else {
            challenge
                .host_public_key
                .as_slice()
                .try_into()
                .map_err(|_| anyhow::anyhow!("AuthChallenge host_public_key must be 32 bytes"))?
        };
        if let Some(tls_pk) = tls_peer_pk
            && !challenge.host_public_key.is_empty()
            && tls_pk != host_pk
        {
            bail!("host public key in AuthChallenge does not match TLS certificate");
        }
        if !challenge.host_device_id.is_empty() {
            let derived = veloxdesk_crypto::device_id_from_public_key(&host_pk);
            if derived != challenge.host_device_id {
                bail!("host device_id does not match host public key");
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
                        (true, true) => "Enter permanent or one-time password".into(),
                        (true, false) => "Enter permanent password".into(),
                        (false, true) => "Enter one-time password".into(),
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

            let _ = proxy.send_event(UserEvent::Session(SessionEvent::Status("Authenticating…".into())));
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
                    .map_err(|e| anyhow::anyhow!("password key derivation failed: {e}"))?;
                Ok(password_proof(&key, &nonce))
            })
            .await
            .context("password derivation task panicked")??;

            return Ok(Some(AuthResponse { method: method as i32, proof: proof.to_vec() }));
        }

        // 3. Interactive approval only (no password required).
        if has(AuthMethod::None) || challenge.methods.is_empty() {
            let _ = proxy.send_event(UserEvent::Session(SessionEvent::Status(
                "Waiting for host to accept…".into(),
            )));
            return Ok(Some(AuthResponse { method: AuthMethod::None as i32, proof: Vec::new() }));
        }

        bail!("host offered no supported authentication methods ({:?})", challenge.methods);
    }

    async fn handshake(
        ctrl_tx: &mut ControlSender,
        ctrl_rx: &mut veloxdesk_transport::ControlReceiver,
        cmd_rx: &mut mpsc::UnboundedReceiver<SessionCommand>,
        id: &DeviceIdentity,
        tls_peer_pk: Option<[u8; 32]>,
        mut cli_password: Option<String>,
        proxy: &EventLoopProxy<UserEvent>,
    ) -> anyhow::Result<HandshakeOutcome> {
        let mut step_timeout = HANDSHAKE_TIMEOUT;
        loop {
            let env = tokio::select! {
                res = timeout(step_timeout, ctrl_rx.recv()) => {
                    res.context("handshake timed out")?.context("reading handshake message")?
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
                        proxy,
                    )
                    .await?
                    else {
                        return Ok(HandshakeOutcome::Cancelled);
                    };
                    ctrl_tx.send(&Msg::AuthResponse(resp).into()).await.context("sending AuthResponse")?;
                    // After responding, host may present an interactive Accept dialog.
                    step_timeout = APPROVAL_TIMEOUT;
                }
                Some(Msg::AuthResult(r)) => {
                    if !r.ok {
                        let reason = if r.reason.is_empty() { "authentication rejected".into() } else { r.reason };
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
                        format!("host rejected session (code {})", c.code)
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
}
