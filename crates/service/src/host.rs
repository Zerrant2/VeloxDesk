//! Host server: QUIC listener, session admission (`max_sessions`), graceful shutdown,
//! embeddable [`RunningHost`] handle, local IPC integration, and wiring to the real
//! `veloxdesk-transport` / `veloxdesk-capture` / `veloxdesk-codec` / `veloxdesk-input` crates.

use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore, broadcast, watch};
use tokio::task::{JoinHandle, JoinSet};
use veloxdesk_core_traits::video::EncodedPacket;
use veloxdesk_core_traits::{CoreError, Result, Timestamp, TransportChannel};
use veloxdesk_crypto::DeviceIdentity;
use veloxdesk_protocol::control::{ChatMessage, Nack, ReceiverReport};
use veloxdesk_transport::{AbrController, Listener, P2pListener, QuicChannel, VideoSender};

use crate::auth::{HostAuthState, TrustedPeer};
use crate::backends::{AudioCaptureFactory, EncoderFactory, HostBackends, RateController, RateDecision, VideoSink};
use crate::config::HostConfig;
use crate::daemon::detect_desktop;
use crate::ipc::{IpcRequest, IpcResponse, IpcServer};
use crate::session::{CLOSE_BUSY, SessionHooks, reject, run_session_with_hooks};

/// Lifecycle events emitted by [`RunningHost`] (consumed by GUI, tray, or daemon observers).
#[derive(Debug, Clone, PartialEq)]
pub enum HostStatusEvent {
    Listening {
        bind: SocketAddr,
        device_id: String,
        otp: Option<String>,
        rendezvous: Option<SocketAddr>,
    },
    SessionStarted {
        peer_device_id: String,
    },
    SessionEnded {
        peer_device_id: String,
        reason: String,
    },
    OtpRotated {
        otp: String,
    },
    TrustedPeerAdded {
        peer: TrustedPeer,
    },
    ChatReceived {
        peer_device_id: String,
        msg: ChatMessage,
    },
    ChatAcked {
        peer_device_id: String,
        id: u64,
    },
    Error(String),
}

/// Optional status callback passed to [`RunningHost::start`].
pub type HostEventCallback = Arc<dyn Fn(HostStatusEvent) + Send + Sync>;

/// Adapts [`veloxdesk_transport::VideoSender`] to [`VideoSink`].
pub struct TransportVideoSink(pub VideoSender);

impl VideoSink for TransportVideoSink {
    fn send_frame(&mut self, pkt: &EncodedPacket) -> Result<u32> {
        Ok(self.0.send_frame(pkt)?.packetize_us)
    }

    fn set_parity_percent(&mut self, pct: u8) {
        self.0.set_parity_percent(pct);
    }

    fn on_nack(&mut self, nack: &Nack) {
        self.0.on_nack(nack);
    }
}

/// Adapts [`veloxdesk_transport::AbrController`] to [`RateController`].
pub struct TransportAbr(pub AbrController);

impl RateController for TransportAbr {
    fn on_report(&mut self, rr: &ReceiverReport, now: Timestamp) -> Option<RateDecision> {
        self.0
            .on_report(rr, now)
            .map(|d| RateDecision { bitrate_kbps: d.bitrate_kbps, parity_percent: d.parity_percent })
    }
}

pub(crate) fn system_audio_capture_factory() -> Arc<AudioCaptureFactory> {
    static FACTORY: OnceLock<Arc<AudioCaptureFactory>> = OnceLock::new();
    FACTORY.get_or_init(|| Arc::new(veloxdesk_audio::create_loopback_capture)).clone()
}

impl HostBackends {
    /// Production capture, input, audio, QUIC FEC sender and ABR, with a caller-provided encoder
    /// factory (useful for tests or custom encoders).
    pub fn with_encoder(encoder: Arc<EncoderFactory>) -> Self {
        Self {
            capture: Arc::new(veloxdesk_capture::create_capture),
            encoder,
            injector: Arc::new(veloxdesk_input::create_injector),
            video_sink: Arc::new(|ch, stream_id| Box::new(TransportVideoSink(VideoSender::new(ch, stream_id)))),
            rate: Arc::new(|start, min, max| Box::new(TransportAbr(AbrController::new(start, min, max)))),
            audio_capture: Some(system_audio_capture_factory()),
            audio_encoder: Some(Arc::new(|cfg| Ok(Box::new(veloxdesk_audio::OpusEncoder::new(cfg)?)))),
            audio_playback: Some(Arc::new(veloxdesk_audio::create_playback)),
            clipboard: Some(Arc::new(veloxdesk_clipboard::create_system_clipboard)),
            privacy: Some(veloxdesk_virtual_display::create_privacy_controller()),
            file_transfer: Some(veloxdesk_file_transfer::FileTransferConfig::default()),
            tunnel_policy: Some(veloxdesk_tunnel::TunnelPolicy::default()),
        }
    }

    /// Production backends for this OS (`veloxdesk-capture`, `veloxdesk-codec`,
    /// `veloxdesk-audio`, `veloxdesk-input`, `veloxdesk-transport`).
    pub fn system() -> Self {
        Self::with_encoder(Arc::new(veloxdesk_codec::create_encoder))
    }
}

enum HostListener {
    Direct(Listener),
    P2p(P2pListener),
}

impl HostListener {
    fn local_addr(&self) -> SocketAddr {
        match self {
            Self::Direct(l) => l.local_addr(),
            Self::P2p(l) => l.local_addr(),
        }
    }

    async fn accept(&self) -> Result<QuicChannel> {
        match self {
            Self::Direct(l) => l.accept().await,
            Self::P2p(l) => l.accept().await,
        }
    }

    fn shutdown(&self) {
        if let Self::P2p(l) = self {
            l.shutdown();
        }
    }
}

/// Handle to a running background VeloxDesk host (embedded in the GUI or owned by `--host`).
///
/// Dropping `RunningHost` automatically signals graceful shutdown.
pub struct RunningHost {
    local_addr: SocketAddr,
    ipc_addr: Option<SocketAddr>,
    device_id: String,
    auth_state: Arc<HostAuthState>,
    active_sessions: Arc<AtomicUsize>,
    chat_tx: broadcast::Sender<ChatMessage>,
    next_chat_id: AtomicU64,
    stop_tx: watch::Sender<bool>,
    task: Option<JoinHandle<Result<()>>>,
}

impl RunningHost {
    /// Start listening for incoming QUIC (or P2P/rendezvous) sessions and an optional local
    /// loopback [`IpcServer`] in background Tokio tasks.
    ///
    /// Both the QUIC listener and (if configured and available) local IPC socket are bound
    /// before this future resolves, so [`Self::local_addr`] and [`Self::ipc_addr`] are valid
    /// immediately. If the configured `cfg.ipc_port` is busy, a warning is logged and the host
    /// continues serving QUIC with `ipc_addr() == None`.
    pub async fn start(
        mut cfg: HostConfig,
        id: DeviceIdentity,
        backends: HostBackends,
        on_event: Option<HostEventCallback>,
    ) -> Result<Self> {
        cfg.validate().map_err(|e| CoreError::invalid(e.to_string()))?;
        cfg.auth.host_public_key.get_or_insert_with(|| id.public_key());

        let auth_state = match cfg.auth.shared_state.clone() {
            Some(s) => s,
            None => {
                let s = HostAuthState::from_config(&cfg.auth);
                cfg.auth.shared_state = Some(s.clone());
                s
            }
        };
        if let Some(cb) = &on_event {
            let cb_otp = cb.clone();
            auth_state.set_on_otp_rotated(Some(Arc::new(move |otp| {
                cb_otp(HostStatusEvent::OtpRotated { otp });
            })));
            let cb_peer = cb.clone();
            auth_state.set_on_trusted_peer_added(Some(Arc::new(move |peer| {
                cb_peer(HostStatusEvent::TrustedPeerAdded { peer });
            })));
        }

        let device_id = id.device_id();
        let listener = if let Some(rendezvous) = cfg.rendezvous {
            let l = P2pListener::bind(cfg.bind, Some(rendezvous), &id).await?;
            tracing::info!(
                device_id = %device_id,
                addr = %l.local_addr(),
                %rendezvous,
                max_fps = cfg.max_fps,
                bitrate_kbps = cfg.bitrate_kbps,
                codec = ?cfg.codec,
                prefer_hw = cfg.prefer_hw,
                "veloxdesk host listening (P2P + direct + relay)"
            );
            HostListener::P2p(l)
        } else {
            let l = veloxdesk_transport::listen(cfg.bind, &id).await?;
            tracing::info!(
                device_id = %device_id,
                addr = %l.local_addr(),
                max_fps = cfg.max_fps,
                bitrate_kbps = cfg.bitrate_kbps,
                codec = ?cfg.codec,
                prefer_hw = cfg.prefer_hw,
                "veloxdesk host listening"
            );
            HostListener::Direct(l)
        };
        let local_addr = listener.local_addr();
        let active_sessions = Arc::new(AtomicUsize::new(0));
        let (stop_tx, stop_rx) = watch::channel(false);
        let (disconnect_tx, disconnect_rx) = watch::channel(0u64);

        let (ipc_addr, ipc_task) = if let Some(port) = cfg.ipc_port {
            let bind_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
            let dev_id = device_id.clone();
            let auth_c = auth_state.clone();
            let active_c = active_sessions.clone();
            let disc_c = disconnect_tx.clone();
            let stop_c = stop_tx.clone();
            let handler = Arc::new(move |req: IpcRequest| match req {
                IpcRequest::Status => IpcResponse::Status {
                    device_id: dev_id.clone(),
                    active_otp: auth_c.current_otp(),
                    active_sessions: active_c.load(Ordering::Relaxed),
                    desktop: detect_desktop().to_string(),
                },
                IpcRequest::RotateOtp => IpcResponse::Otp { otp: auth_c.rotate_otp() },
                IpcRequest::DisconnectAll => {
                    disc_c.send_modify(|epoch| *epoch = epoch.wrapping_add(1));
                    IpcResponse::Ok { message: "disconnecting active sessions".into() }
                }
                IpcRequest::Shutdown => {
                    let _ = stop_c.send(true);
                    IpcResponse::Ok { message: "shutting down".into() }
                }
            });
            match IpcServer::bind(bind_addr, handler).await {
                Ok(srv) => {
                    let addr = srv.local_addr();
                    tracing::info!(addr = %addr, "veloxdesk local IPC listening");
                    let rx = stop_rx.clone();
                    let handle = tokio::spawn(async move {
                        if let Err(e) = srv.run_until(rx).await {
                            tracing::warn!("local IPC server exited with error: {e}");
                        }
                    });
                    (Some(addr), Some(handle))
                }
                Err(e) => {
                    tracing::warn!(port, "local IPC bind failed (continuing without IPC): {e}");
                    (None, None)
                }
            }
        } else {
            (None, None)
        };

        let initial_otp = auth_state.current_otp();
        if let Some(otp) = &initial_otp {
            tracing::info!(otp = %otp, "veloxdesk host one-time password active");
        }
        if let Some(cb) = &on_event {
            cb(HostStatusEvent::Listening {
                bind: local_addr,
                device_id: device_id.clone(),
                otp: initial_otp,
                rendezvous: cfg.rendezvous,
            });
        }

        let (chat_tx, _) = broadcast::channel::<ChatMessage>(64);
        let hooks = SessionHooks {
            active_sessions: Some(active_sessions.clone()),
            on_event: on_event.clone(),
            chat_tx: Some(chat_tx.clone()),
            power_executor: None,
        };
        let stop_tx_done = stop_tx.clone();
        let mut accept_stop = stop_rx;
        let task = tokio::spawn(async move {
            let res = serve_listener_core(
                listener,
                cfg,
                backends,
                wait_watch_true(&mut accept_stop),
                disconnect_rx,
                hooks,
            )
            .await;
            let _ = stop_tx_done.send(true);
            if let Some(ipc) = ipc_task {
                let _ = tokio::time::timeout(Duration::from_secs(2), ipc).await;
            }
            if let Err(ref e) = res
                && let Some(cb) = &on_event
            {
                cb(HostStatusEvent::Error(e.to_string()));
            }
            res
        });

        Ok(Self {
            local_addr,
            ipc_addr,
            device_id,
            auth_state,
            active_sessions,
            chat_tx,
            next_chat_id: AtomicU64::new(1),
            stop_tx,
            task: Some(task),
        })
    }

    /// Local UDP address the QUIC/P2P listener is bound to.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Local loopback TCP address of the live [`IpcServer`], if enabled and bound.
    pub fn ipc_addr(&self) -> Option<SocketAddr> {
        self.ipc_addr
    }

    /// 9-digit Device ID of this running host.
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// Live shared authentication state (`current_otp()`, `rotate_otp()`, `trusted_peers()`,
    /// `add_trusted_peer()`, `set_require_accept()`).
    pub fn auth_state(&self) -> Arc<HostAuthState> {
        self.auth_state.clone()
    }

    /// Number of currently authenticated, active viewer sessions.
    pub fn active_sessions(&self) -> usize {
        self.active_sessions.load(Ordering::Relaxed)
    }

    /// Broadcast a chat message from the local host user to all active viewer sessions (v0.5.0).
    /// Returns the assigned monotonic message ID (starting at 1).
    pub fn send_chat(&self, text: String, sender_name: String) -> Result<u64> {
        let id = self.next_chat_id.fetch_add(1, Ordering::Relaxed);
        let ts_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let msg = ChatMessage { id, sender_name, text, ts_ms };
        // Ignore SendError when no viewer session is currently subscribed.
        let _ = self.chat_tx.send(msg);
        Ok(id)
    }

    /// Signal the host listener, active sessions, and local IPC server to shut down gracefully.
    pub fn stop(&self) {
        let _ = self.stop_tx.send(true);
    }

    /// Wait for the background host tasks to finish draining after [`Self::stop`] (or IPC shutdown).
    pub async fn join(mut self) -> Result<()> {
        if let Some(task) = self.task.take() {
            match task.await {
                Ok(res) => res,
                Err(e) => Err(CoreError::platform(format!("host task panicked: {e}"))),
            }
        } else {
            Ok(())
        }
    }
}

impl Drop for RunningHost {
    fn drop(&mut self) {
        let _ = self.stop_tx.send(true);
    }
}

async fn wait_watch_true(rx: &mut watch::Receiver<bool>) {
    while !*rx.borrow() {
        if rx.changed().await.is_err() {
            break;
        }
    }
}

/// Accept sessions on a direct QUIC `listener` until `shutdown` resolves (or the listener closes).
///
/// Enforces `cfg.max_sessions`, rejecting excess connections with [`CLOSE_BUSY`]. When
/// `shutdown` fires, active sessions are signalled to send `Close`, stop their threads
/// and release all pressed keys before returning.
pub async fn serve_until(
    listener: Listener,
    cfg: HostConfig,
    backends: HostBackends,
    shutdown: impl Future<Output = ()>,
) -> Result<()> {
    let (_disc_tx, disc_rx) = watch::channel(0u64);
    serve_listener_core(
        HostListener::Direct(listener),
        cfg,
        backends,
        shutdown,
        disc_rx,
        SessionHooks::default(),
    )
    .await
}

/// Accept sessions on a multiplexed [`P2pListener`] (direct LAN + NAT hole-punched P2P +
/// zero-knowledge relay) until `shutdown` resolves.
pub async fn serve_p2p_until(
    listener: P2pListener,
    cfg: HostConfig,
    backends: HostBackends,
    shutdown: impl Future<Output = ()>,
) -> Result<()> {
    let (_disc_tx, disc_rx) = watch::channel(0u64);
    serve_listener_core(
        HostListener::P2p(listener),
        cfg,
        backends,
        shutdown,
        disc_rx,
        SessionHooks::default(),
    )
    .await
}

const SUPERSEDE_WAIT_TIMEOUT: Duration = Duration::from_secs(2);

struct ActiveSessionEntry {
    session_id: u64,
    peer_pk: Option<[u8; 32]>,
    stop_tx: watch::Sender<bool>,
}

struct ActiveSessionGuard {
    session_id: u64,
    registry: Arc<Mutex<Vec<ActiveSessionEntry>>>,
}

impl ActiveSessionGuard {
    fn register(
        registry: Arc<Mutex<Vec<ActiveSessionEntry>>>,
        session_id: u64,
        peer_pk: Option<[u8; 32]>,
        stop_tx: watch::Sender<bool>,
    ) -> Self {
        registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(ActiveSessionEntry { session_id, peer_pk, stop_tx });
        Self { session_id, registry }
    }
}

impl Drop for ActiveSessionGuard {
    fn drop(&mut self) {
        self.registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|e| e.session_id != self.session_id);
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_registered_session(
    ch: Arc<dyn TransportChannel>,
    permit: OwnedSemaphorePermit,
    registry: Arc<Mutex<Vec<ActiveSessionEntry>>>,
    entry_id: u64,
    cfg: Arc<HostConfig>,
    backends: HostBackends,
    mut host_stop: watch::Receiver<bool>,
    mut disc: watch::Receiver<u64>,
    hooks: SessionHooks,
) {
    let _permit = permit;
    let (sess_stop_tx, sess_stop_rx) = watch::channel(false);
    // Registered AFTER `_permit` so `_reg_guard` drops BEFORE `_permit` is released,
    // ensuring a waiting superseding session sees a clean registry as soon as it acquires the permit.
    let _reg_guard = ActiveSessionGuard::register(registry, entry_id, ch.peer_public_key(), sess_stop_tx.clone());
    let fwd = tokio::spawn(async move {
        tokio::select! {
            () = wait_watch_true(&mut host_stop) => {
                let _ = sess_stop_tx.send(true);
            }
            res = disc.changed() => {
                if res.is_ok() {
                    let _ = sess_stop_tx.send(true);
                }
            }
            () = sess_stop_tx.closed() => {}
        }
    });
    match run_session_with_hooks(ch, cfg, backends, sess_stop_rx, hooks).await {
        Ok(s) => tracing::info!(
            session = s.session_id,
            peer = %s.peer_device_id,
            frames = s.frames_sent,
            dropped = s.frames_dropped,
            input = s.input_events,
            reason = %s.close_reason,
            "session finished"
        ),
        Err(e) => tracing::warn!("session error: {e}"),
    }
    fwd.abort();
}

async fn serve_listener_core(
    listener: HostListener,
    mut cfg: HostConfig,
    backends: HostBackends,
    shutdown: impl Future<Output = ()>,
    disconnect_rx: watch::Receiver<u64>,
    hooks: SessionHooks,
) -> Result<()> {
    cfg.validate().map_err(|e| CoreError::invalid(e.to_string()))?;
    if !cfg.auth.is_open() {
        let state = match cfg.auth.shared_state.clone() {
            Some(s) => s,
            None => {
                let s = HostAuthState::from_config(&cfg.auth);
                cfg.auth.shared_state = Some(s.clone());
                s
            }
        };
        if let Some(otp) = state.current_otp() {
            tracing::info!(otp = %otp, "veloxdesk host one-time password active");
        }
    }
    let cfg = Arc::new(cfg);
    let permits = Arc::new(Semaphore::new(cfg.max_sessions));
    let active_registry: Arc<Mutex<Vec<ActiveSessionEntry>>> = Arc::new(Mutex::new(Vec::new()));
    let next_entry_id = Arc::new(AtomicU64::new(1));
    let (stop_tx, stop_rx) = watch::channel(false);
    let mut sessions = JoinSet::new();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            () = &mut shutdown => {
                tracing::info!("host shutdown requested");
                break;
            }
            Some(res) = sessions.join_next(), if !sessions.is_empty() => {
                if let Err(e) = res {
                    tracing::warn!("session task panicked: {e}");
                }
            }
            accepted = listener.accept() => {
                match accepted {
                    Ok(ch) => {
                        let ch: Arc<dyn TransportChannel> = Arc::new(ch);
                        match permits.clone().try_acquire_owned() {
                            Ok(permit) => {
                                let entry_id = next_entry_id.fetch_add(1, Ordering::Relaxed);
                                let cfg = cfg.clone();
                                let backends = backends.clone();
                                let host_stop = stop_rx.clone();
                                let mut disc = disconnect_rx.clone();
                                let _ = disc.borrow_and_update();
                                let hooks = hooks.clone();
                                let registry = active_registry.clone();
                                // Pre-register immediately so a rapid reconnect sees this session before spawn runs.
                                let (sess_stop_tx, sess_stop_rx) = watch::channel(false);
                                let reg_guard = ActiveSessionGuard::register(
                                    registry,
                                    entry_id,
                                    ch.peer_public_key(),
                                    sess_stop_tx.clone(),
                                );
                                sessions.spawn(async move {
                                    let _permit = permit;
                                    let _reg_guard = reg_guard;
                                    let mut host_stop = host_stop;
                                    let fwd = tokio::spawn(async move {
                                        tokio::select! {
                                            () = wait_watch_true(&mut host_stop) => {
                                                let _ = sess_stop_tx.send(true);
                                            }
                                            res = disc.changed() => {
                                                if res.is_ok() {
                                                    let _ = sess_stop_tx.send(true);
                                                }
                                            }
                                            () = sess_stop_tx.closed() => {}
                                        }
                                    });
                                    match run_session_with_hooks(ch, cfg, backends, sess_stop_rx, hooks).await {
                                        Ok(s) => tracing::info!(
                                            session = s.session_id,
                                            peer = %s.peer_device_id,
                                            frames = s.frames_sent,
                                            dropped = s.frames_dropped,
                                            input = s.input_events,
                                            reason = %s.close_reason,
                                            "session finished"
                                        ),
                                        Err(e) => tracing::warn!("session error: {e}"),
                                    }
                                    fwd.abort();
                                });
                            }
                            Err(_) => {
                                let incoming_pk = ch.peer_public_key();
                                let superseded = if let Some(pk) = incoming_pk {
                                    let g = active_registry.lock().unwrap_or_else(|e| e.into_inner());
                                    let mut matched = false;
                                    for entry in g.iter() {
                                        if entry.peer_pk == Some(pk) {
                                            let _ = entry.stop_tx.send(true);
                                            matched = true;
                                        }
                                    }
                                    matched
                                } else {
                                    false
                                };

                                if superseded {
                                    tracing::info!(
                                        "same peer reconnected while max_sessions ({}) reached; superseding stale session",
                                        cfg.max_sessions
                                    );
                                    let entry_id = next_entry_id.fetch_add(1, Ordering::Relaxed);
                                    let permits_c = permits.clone();
                                    let registry = active_registry.clone();
                                    let cfg = cfg.clone();
                                    let backends = backends.clone();
                                    let mut wait_stop = stop_rx.clone();
                                    let host_stop = stop_rx.clone();
                                    let mut disc = disconnect_rx.clone();
                                    let _ = disc.borrow_and_update();
                                    let hooks = hooks.clone();
                                    sessions.spawn(async move {
                                        let acquired = tokio::select! {
                                            () = wait_watch_true(&mut wait_stop) => None,
                                            res = tokio::time::timeout(SUPERSEDE_WAIT_TIMEOUT, permits_c.acquire_owned()) => {
                                                res.ok().and_then(std::result::Result::ok)
                                            }
                                        };
                                        if let Some(permit) = acquired {
                                            run_registered_session(
                                                ch, permit, registry, entry_id, cfg, backends, host_stop, disc, hooks,
                                            )
                                            .await;
                                        } else if *wait_stop.borrow() {
                                            ch.close(CLOSE_BUSY, "host stopping");
                                        } else {
                                            tracing::warn!("timed out waiting to supersede stale session; rejecting with CLOSE_BUSY");
                                            reject(ch, CLOSE_BUSY, "host busy").await;
                                        }
                                    });
                                } else {
                                    tracing::info!("rejecting viewer: max_sessions ({}) reached", cfg.max_sessions);
                                    tokio::spawn(async move {
                                        reject(ch, CLOSE_BUSY, "host busy").await;
                                    });
                                }
                            }
                        }
                    }
                    Err(CoreError::Closed) => break,
                    Err(e) => {
                        tracing::warn!("listener accept error: {e}");
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                }
            }
        }
    }

    // Signal active sessions to close cleanly (stops capture/encode threads + release_all).
    listener.shutdown();
    let _ = stop_tx.send(true);
    let drain = async {
        while sessions.join_next().await.is_some() {}
    };
    if tokio::time::timeout(Duration::from_secs(3), drain).await.is_err() {
        tracing::warn!("timed out waiting for sessions to exit; aborting remaining");
        sessions.abort_all();
    }
    Ok(())
}

/// Listen on `cfg.bind` with `backends` (and live local IPC when `cfg.ipc_port.is_some()`)
/// until `shutdown` resolves or an IPC `Shutdown` command is received.
///
/// Uses [`P2pListener`] when `cfg.rendezvous` is set (registering the host's 9-digit Device ID
/// and handling direct LAN, NAT hole punching, and zero-knowledge relay on one UDP socket),
/// or a direct QUIC [`Listener`] otherwise.
pub async fn run_host_with_backends(
    cfg: HostConfig,
    id: DeviceIdentity,
    backends: HostBackends,
    shutdown: impl Future<Output = ()>,
) -> Result<()> {
    let host = RunningHost::start(cfg, id, backends, None).await?;
    let mut stop_rx = host.stop_tx.subscribe();
    tokio::pin!(shutdown);
    tokio::select! {
        () = &mut shutdown => {
            host.stop();
        }
        () = wait_watch_true(&mut stop_rx) => {}
    }
    host.join().await
}

/// Start the VeloxDesk host: listen for QUIC sessions (plus local IPC) and stream
/// capture → encode → FEC datagrams while injecting remote input.
pub async fn run_host(cfg: HostConfig, id: DeviceIdentity) -> Result<()> {
    run_host_with_backends(cfg, id, HostBackends::system(), std::future::pending()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use veloxdesk_core_traits::video::{ChromaFormat, ColorSpace, EncoderConfig, PixelFormat, RateControl, VideoCodec};
    use veloxdesk_crypto::auth::{derive_password_key, password_proof};
    use veloxdesk_protocol::StreamKind;
    use veloxdesk_protocol::control::{AuthMethod, AuthResponse, Hello, envelope::Msg};
    use veloxdesk_transport::{connect, framed};

    #[test]
    fn system_backends_creates_encoder() {
        let backends = HostBackends::system();
        let cfg = EncoderConfig {
            codec: VideoCodec::H264,
            width: 320,
            height: 180,
            fps: 60,
            bitrate_kbps: 2_000,
            rate_control: RateControl::Cbr,
            chroma: ChromaFormat::Yuv420,
            bit_depth: 8,
            gop: None,
            intra_refresh: false,
            input_format: PixelFormat::Bgra8,
            gpu_input: false,
            color_space: ColorSpace::Bt709,
        };
        let enc = (backends.encoder)(&cfg, false).expect("system SW encoder");
        assert!(enc.caps().codecs.contains(&VideoCodec::H264));
    }

    /// Minimal synthetic backends for deterministic `RunningHost` loopback tests.
    fn synthetic_backends() -> HostBackends {
        use bytes::Bytes;
        use veloxdesk_core_traits::input::{DisplayGeometry, InputCaps, InputEvent, InputFeedback};
        use veloxdesk_core_traits::video::{
            CaptureCaps, CaptureConfig, CapturedFrame, ColorRange, Display, DisplayId, EncodeOptions, EncodedPacket,
            EncoderCaps, EncoderUpdate, Frame, FrameData, Plane,
        };
        use veloxdesk_core_traits::{FrameCapture, InputInjector, VideoEncoder};

        struct StubCap;
        impl FrameCapture for StubCap {
            fn caps(&self) -> CaptureCaps {
                CaptureCaps { backend: "stub", formats: vec![PixelFormat::Bgra8], ..Default::default() }
            }
            fn displays(&self) -> Result<Vec<Display>> {
                Ok(vec![Display {
                    id: DisplayId("D0".into()),
                    index: 0,
                    name: "Stub".into(),
                    x: 0,
                    y: 0,
                    width: 64,
                    height: 32,
                    refresh_hz: 60,
                    scale_factor: 1.0,
                    primary: true,
                    hdr: false,
                }])
            }
            fn start(&mut self, _cfg: &CaptureConfig) -> Result<()> {
                Ok(())
            }
            fn next_frame(&mut self, timeout: Duration) -> Result<Option<CapturedFrame>> {
                std::thread::sleep(timeout.min(Duration::from_millis(10)));
                Ok(Some(CapturedFrame {
                    frame: Frame {
                        width: 64,
                        height: 32,
                        format: PixelFormat::Bgra8,
                        color_space: ColorSpace::Srgb,
                        color_range: ColorRange::Full,
                        data: FrameData::Cpu(vec![Plane { data: Bytes::from(vec![0u8; 64 * 32 * 4]), stride: 256 }]),
                        capture_ts: Timestamp::now(),
                    },
                    dirty_rects: Vec::new(),
                    cursor: None,
                    capture_us: 10,
                    convert_us: 0,
                }))
            }
            fn stop(&mut self) {}
        }

        struct StubEnc(EncoderConfig, u32);
        impl VideoEncoder for StubEnc {
            fn caps(&self) -> EncoderCaps {
                EncoderCaps { name: "stub", codecs: vec![VideoCodec::H264], ..Default::default() }
            }
            fn config(&self) -> &EncoderConfig {
                &self.0
            }
            fn encode(&mut self, frame: &Frame, opts: EncodeOptions) -> Result<Option<EncodedPacket>> {
                self.1 += 1;
                Ok(Some(EncodedPacket {
                    data: Bytes::from_static(&[0, 0, 0, 1, 0x65, 0x88]),
                    codec: self.0.codec,
                    keyframe: opts.force_keyframe || self.1 == 1,
                    recovery_point: false,
                    width: frame.width,
                    height: frame.height,
                    capture_ts: frame.capture_ts,
                    frame_id: self.1,
                    encode_us: 10,
                    convert_us: 0,
                }))
            }
            fn update(&mut self, _u: EncoderUpdate) -> Result<()> {
                Ok(())
            }
        }

        struct StubInj;
        impl InputInjector for StubInj {
            fn caps(&self) -> InputCaps {
                InputCaps::default()
            }
            fn set_displays(&mut self, _d: &[DisplayGeometry]) {}
            fn inject(&mut self, _ev: &InputEvent) -> Result<()> {
                Ok(())
            }
            fn release_all(&mut self) -> Result<()> {
                Ok(())
            }
            fn poll_feedback(&mut self) -> Vec<InputFeedback> {
                Vec::new()
            }
        }

        HostBackends {
            capture: Arc::new(|_| Ok(Box::new(StubCap))),
            encoder: Arc::new(|cfg, _| Ok(Box::new(StubEnc(cfg.clone(), 0)))),
            injector: Arc::new(|| Ok(Box::new(StubInj))),
            video_sink: Arc::new(|ch, sid| Box::new(TransportVideoSink(VideoSender::new(ch, sid)))),
            rate: Arc::new(|s, mi, ma| Box::new(TransportAbr(AbrController::new(s, mi, ma)))),
            audio_capture: None,
            audio_encoder: None,
            audio_playback: None,
            clipboard: None,
            privacy: None,
            file_transfer: None,
            tunnel_policy: None,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn running_host_live_ipc_otp_session_and_disconnect() {
        let host_id = DeviceIdentity::generate();
        let expected_device_id = host_id.device_id();
        let expected_host_pk = host_id.public_key();
        let viewer_id = DeviceIdentity::generate();

        let events: Arc<Mutex<Vec<HostStatusEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let events_c = events.clone();

        let mut cfg = HostConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            ipc_port: Some(0),
            audio_enabled: false,
            ..Default::default()
        };
        cfg.auth = crate::auth::HostAuthConfig::with_auto_otp();

        let host = RunningHost::start(
            cfg,
            host_id,
            synthetic_backends(),
            Some(Arc::new(move |ev| events_c.lock().unwrap().push(ev))),
        )
        .await
        .expect("RunningHost::start");

        let ipc_addr = host.ipc_addr().expect("IPC bound on ephemeral loopback port");
        let initial_otp = host.auth_state().current_otp().expect("auto_otp active");

        // 1. Live IPC Status returns real device_id, OTP, 0 sessions, desktop.
        let st = crate::ipc::IpcClient::request(ipc_addr, &IpcRequest::Status).await.unwrap();
        match st {
            IpcResponse::Status { device_id, active_otp, active_sessions, desktop } => {
                assert_eq!(device_id, expected_device_id);
                assert_eq!(active_otp.as_deref(), Some(initial_otp.as_str()));
                assert_eq!(active_sessions, 0);
                assert!(!desktop.is_empty());
            }
            other => panic!("expected Status, got {other:?}"),
        }

        // 2. Live IPC RotateOtp updates auth_state and fires OtpRotated event.
        let rot = crate::ipc::IpcClient::request(ipc_addr, &IpcRequest::RotateOtp).await.unwrap();
        let IpcResponse::Otp { otp: rotated_otp } = rot else { panic!("expected Otp") };
        assert_ne!(rotated_otp, initial_otp);
        assert_eq!(host.auth_state().current_otp().as_deref(), Some(rotated_otp.as_str()));

        // 3. Connect viewer over real QUIC using the newly rotated OTP.
        let ch = connect(host.local_addr(), &viewer_id, Some(expected_host_pk)).await.expect("connect");
        let (send, recv) = ch.open_bi(StreamKind::Control).await.unwrap();
        let (mut writer, mut reader) = framed(send, recv);
        writer
            .send(
                &Msg::Hello(Hello {
                    protocol_version: veloxdesk_protocol::PROTOCOL_VERSION,
                    device_id: viewer_id.device_id(),
                    device_name: "test-viewer".into(),
                    os: "windows".into(),
                    public_key: viewer_id.public_key().to_vec(),
                    ..Default::default()
                })
                .into(),
            )
            .await
            .unwrap();

        let ch_msg = tokio::time::timeout(Duration::from_secs(5), reader.recv()).await.unwrap().unwrap();
        let Some(Msg::AuthChallenge(challenge)) = ch_msg.msg else {
            panic!("expected AuthChallenge, got {:?}", ch_msg.msg);
        };
        let params = veloxdesk_crypto::auth::Argon2Params {
            m_cost_kib: challenge.argon2_m_cost_kib,
            t_cost: challenge.argon2_t_cost,
            p_cost: challenge.argon2_p_cost,
        };
        let key = derive_password_key(&rotated_otp, &challenge.salt, params).unwrap();
        let nonce: [u8; 32] = challenge.nonce.as_slice().try_into().unwrap();
        let proof = password_proof(&key, &nonce);
        writer
            .send(&Msg::AuthResponse(AuthResponse { method: AuthMethod::OneTimePassword as i32, proof: proof.to_vec() }).into())
            .await
            .unwrap();

        let res_msg = tokio::time::timeout(Duration::from_secs(5), reader.recv()).await.unwrap().unwrap();
        let Some(Msg::AuthResult(res)) = res_msg.msg else {
            panic!("expected AuthResult, got {:?}", res_msg.msg);
        };
        assert!(res.ok, "OTP auth failed: {}", res.reason);

        // Wait for SessionConfig so the session is fully active.
        let cfg_msg = tokio::time::timeout(Duration::from_secs(5), reader.recv()).await.unwrap().unwrap();
        assert!(matches!(cfg_msg.msg, Some(Msg::SessionConfig(_))));

        // 4. Live IPC Status now reports active_sessions == 1 and OTP was auto-rotated again.
        let st2 = crate::ipc::IpcClient::request(ipc_addr, &IpcRequest::Status).await.unwrap();
        match st2 {
            IpcResponse::Status { active_sessions, active_otp, .. } => {
                assert_eq!(active_sessions, 1);
                assert_ne!(active_otp.as_deref(), Some(rotated_otp.as_str()));
            }
            other => panic!("expected Status, got {other:?}"),
        }
        assert_eq!(host.active_sessions(), 1);

        // 5. DisconnectAll closes the active session without stopping the host.
        let disc = crate::ipc::IpcClient::request(ipc_addr, &IpcRequest::DisconnectAll).await.unwrap();
        assert!(matches!(disc, IpcResponse::Ok { .. }));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while host.active_sessions() > 0 && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(host.active_sessions(), 0);

        // 6. Stop cleanly shuts down RunningHost and drains background tasks.
        host.stop();
        tokio::time::timeout(Duration::from_secs(5), host.join()).await.unwrap().unwrap();

        let recorded = events.lock().unwrap();
        assert!(recorded.iter().any(|e| matches!(e, HostStatusEvent::Listening { .. })));
        assert!(recorded.iter().any(|e| matches!(e, HostStatusEvent::OtpRotated { .. })));
        assert!(recorded.iter().any(|e| matches!(e, HostStatusEvent::SessionStarted { .. })));
        assert!(recorded.iter().any(|e| matches!(e, HostStatusEvent::SessionEnded { .. })));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn running_host_emits_trusted_peer_added_event() {
        let host_id = DeviceIdentity::generate();
        let expected_host_pk = host_id.public_key();
        let viewer_id = DeviceIdentity::generate();
        let expected_viewer_pk = viewer_id.public_key();

        let events: Arc<Mutex<Vec<HostStatusEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let events_c = events.clone();

        let approval: crate::auth::ApprovalHandler =
            Arc::new(|req| Box::pin(async move { crate::auth::ApprovalDecision::accept_and_trust(req.default_permissions) }));

        let cfg = HostConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            ipc_port: None,
            audio_enabled: false,
            auth: crate::auth::HostAuthConfig::require_accept_with(approval),
            ..Default::default()
        };

        let host = RunningHost::start(
            cfg,
            host_id,
            synthetic_backends(),
            Some(Arc::new(move |ev| events_c.lock().unwrap().push(ev))),
        )
        .await
        .expect("RunningHost::start");

        let ch = connect(host.local_addr(), &viewer_id, Some(expected_host_pk)).await.expect("connect");
        let (send, recv) = ch.open_bi(StreamKind::Control).await.unwrap();
        let (mut writer, mut reader) = framed(send, recv);
        writer
            .send(
                &Msg::Hello(Hello {
                    protocol_version: veloxdesk_protocol::PROTOCOL_VERSION,
                    device_id: viewer_id.device_id(),
                    device_name: "trusted-laptop".into(),
                    os: "windows".into(),
                    public_key: viewer_id.public_key().to_vec(),
                    ..Default::default()
                })
                .into(),
            )
            .await
            .unwrap();

        let ch_msg = tokio::time::timeout(Duration::from_secs(5), reader.recv()).await.unwrap().unwrap();
        assert!(matches!(ch_msg.msg, Some(Msg::AuthChallenge(_))));
        writer
            .send(&Msg::AuthResponse(AuthResponse { method: AuthMethod::None as i32, proof: Vec::new() }).into())
            .await
            .unwrap();

        let res_msg = tokio::time::timeout(Duration::from_secs(5), reader.recv()).await.unwrap().unwrap();
        let Some(Msg::AuthResult(res)) = res_msg.msg else { panic!("expected AuthResult") };
        assert!(res.ok);

        host.stop();
        tokio::time::timeout(Duration::from_secs(5), host.join()).await.unwrap().unwrap();

        let recorded = events.lock().unwrap();
        assert!(
            recorded.iter().any(|e| matches!(
                e,
                HostStatusEvent::TrustedPeerAdded { peer }
                    if peer.public_key == expected_viewer_pk && peer.name == "trusted-laptop"
            )),
            "expected TrustedPeerAdded event, got {recorded:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn running_host_bidirectional_chat_and_ack() {
        use veloxdesk_protocol::control::ChatAck;

        let host_id = DeviceIdentity::generate();
        let expected_host_pk = host_id.public_key();
        let viewer_id = DeviceIdentity::generate();

        let events: Arc<Mutex<Vec<HostStatusEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let events_c = events.clone();

        let cfg = HostConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            ipc_port: None,
            audio_enabled: false,
            ..Default::default()
        };

        let host = RunningHost::start(
            cfg,
            host_id,
            synthetic_backends(),
            Some(Arc::new(move |ev| events_c.lock().unwrap().push(ev))),
        )
        .await
        .expect("RunningHost::start");

        let ch = connect(host.local_addr(), &viewer_id, Some(expected_host_pk)).await.expect("connect");
        let (send, recv) = ch.open_bi(StreamKind::Control).await.unwrap();
        let (mut writer, mut reader) = framed(send, recv);
        writer
            .send(
                &Msg::Hello(Hello {
                    protocol_version: veloxdesk_protocol::PROTOCOL_VERSION,
                    device_id: viewer_id.device_id(),
                    device_name: "chat-viewer".into(),
                    os: "windows".into(),
                    public_key: viewer_id.public_key().to_vec(),
                    ..Default::default()
                })
                .into(),
            )
            .await
            .unwrap();

        // Drain AuthResult + SessionConfig.
        let mut saw_cfg = false;
        for _ in 0..8 {
            let env = tokio::time::timeout(Duration::from_secs(5), reader.recv()).await.unwrap().unwrap();
            if matches!(env.msg, Some(Msg::SessionConfig(_))) {
                saw_cfg = true;
                break;
            }
        }
        assert!(saw_cfg, "expected SessionConfig");

        // 1. Viewer -> Host chat: host emits ChatReceived and replies ChatAck.
        writer
            .send(
                &Msg::ChatMessage(ChatMessage {
                    id: 77,
                    sender_name: "Viewer".into(),
                    text: "Hello host!".into(),
                    ts_ms: 1_700_000_000_000,
                })
                .into(),
            )
            .await
            .unwrap();

        let ack = loop {
            let env = tokio::time::timeout(Duration::from_secs(5), reader.recv()).await.unwrap().unwrap();
            if let Some(Msg::ChatAck(a)) = env.msg {
                break a;
            }
        };
        assert_eq!(ack.id, 77);

        // 2. Host -> Viewer chat via RunningHost::send_chat assigns monotonic IDs starting at 1.
        let id1 = host.send_chat("Reply from host".into(), "HostAdmin".into()).unwrap();
        let id2 = host.send_chat("Second line".into(), "HostAdmin".into()).unwrap();
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);

        let mut received_from_host = Vec::new();
        while received_from_host.len() < 2 {
            let env = tokio::time::timeout(Duration::from_secs(5), reader.recv()).await.unwrap().unwrap();
            if let Some(Msg::ChatMessage(m)) = env.msg {
                received_from_host.push(m);
            }
        }
        assert_eq!(received_from_host[0].id, 1);
        assert_eq!(received_from_host[0].sender_name, "HostAdmin");
        assert_eq!(received_from_host[0].text, "Reply from host");
        assert!(received_from_host[0].ts_ms > 0);
        assert_eq!(received_from_host[1].id, 2);

        // Viewer acknowledges host message -> host emits ChatAcked.
        writer.send(&Msg::ChatAck(ChatAck { id: id1 }).into()).await.unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            if events.lock().unwrap().iter().any(|e| matches!(e, HostStatusEvent::ChatAcked { id: 1, .. })) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
        }

        host.stop();
        tokio::time::timeout(Duration::from_secs(5), host.join()).await.unwrap().unwrap();

        let recorded = events.lock().unwrap();
        assert!(
            recorded.iter().any(|e| matches!(
                e,
                HostStatusEvent::ChatReceived { msg, .. } if msg.id == 77 && msg.text == "Hello host!"
            )),
            "expected ChatReceived event, got {recorded:?}"
        );
        assert!(
            recorded.iter().any(|e| matches!(e, HostStatusEvent::ChatAcked { id: 1, .. })),
            "expected ChatAcked event, got {recorded:?}"
        );
    }
}
