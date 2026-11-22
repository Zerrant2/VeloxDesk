//! Host server: QUIC listener, session admission (`max_sessions`), graceful shutdown,
//! and wiring to the real `veloxdesk-transport` / `veloxdesk-capture` / `veloxdesk-codec` /
//! `veloxdesk-input` crates.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Semaphore, watch};
use tokio::task::JoinSet;
use veloxdesk_core_traits::video::EncodedPacket;
use veloxdesk_core_traits::{CoreError, Result, Timestamp, TransportChannel};
use veloxdesk_crypto::DeviceIdentity;
use veloxdesk_protocol::control::{Nack, ReceiverReport};
use veloxdesk_transport::{AbrController, Listener, P2pListener, QuicChannel, VideoSender};

use crate::auth::HostAuthState;
use crate::backends::{EncoderFactory, HostBackends, RateController, RateDecision, VideoSink};
use crate::config::HostConfig;
use crate::session::{CLOSE_BUSY, reject, run_session_until};

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
            audio_capture: Some(Arc::new(veloxdesk_audio::create_loopback_capture)),
            audio_encoder: Some(Arc::new(|cfg| Ok(Box::new(veloxdesk_audio::OpusEncoder::new(cfg)?)))),
            audio_playback: Some(Arc::new(veloxdesk_audio::create_playback)),
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
    serve_listener_until(HostListener::Direct(listener), cfg, backends, shutdown).await
}

/// Accept sessions on a multiplexed [`P2pListener`] (direct LAN + NAT hole-punched P2P +
/// zero-knowledge relay) until `shutdown` resolves.
pub async fn serve_p2p_until(
    listener: P2pListener,
    cfg: HostConfig,
    backends: HostBackends,
    shutdown: impl Future<Output = ()>,
) -> Result<()> {
    serve_listener_until(HostListener::P2p(listener), cfg, backends, shutdown).await
}

async fn serve_listener_until(
    listener: HostListener,
    mut cfg: HostConfig,
    backends: HostBackends,
    shutdown: impl Future<Output = ()>,
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
                                let cfg = cfg.clone();
                                let backends = backends.clone();
                                let rx = stop_rx.clone();
                                sessions.spawn(async move {
                                    let _permit = permit;
                                    match run_session_until(ch, cfg, backends, rx).await {
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
                                });
                            }
                            Err(_) => {
                                tracing::info!("rejecting viewer: max_sessions ({}) reached", cfg.max_sessions);
                                tokio::spawn(async move {
                                    reject(ch, CLOSE_BUSY, "host busy").await;
                                });
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

/// Listen on `cfg.bind` with `backends` until `shutdown` resolves.
///
/// Uses [`P2pListener`] when `cfg.rendezvous` is set (registering the host's 9-digit Device ID
/// and handling direct LAN, NAT hole punching, and zero-knowledge relay on one UDP socket),
/// or a direct QUIC [`Listener`] otherwise.
pub async fn run_host_with_backends(
    mut cfg: HostConfig,
    id: DeviceIdentity,
    backends: HostBackends,
    shutdown: impl Future<Output = ()>,
) -> Result<()> {
    cfg.validate().map_err(|e| CoreError::invalid(e.to_string()))?;
    cfg.auth.host_public_key.get_or_insert_with(|| id.public_key());
    if let Some(rendezvous) = cfg.rendezvous {
        let listener = P2pListener::bind(cfg.bind, Some(rendezvous), &id).await?;
        tracing::info!(
            device_id = %id.device_id(),
            addr = %listener.local_addr(),
            %rendezvous,
            max_fps = cfg.max_fps,
            bitrate_kbps = cfg.bitrate_kbps,
            codec = ?cfg.codec,
            prefer_hw = cfg.prefer_hw,
            "veloxdesk host listening (P2P + direct + relay)"
        );
        serve_p2p_until(listener, cfg, backends, shutdown).await
    } else {
        let listener = veloxdesk_transport::listen(cfg.bind, &id).await?;
        tracing::info!(
            device_id = %id.device_id(),
            addr = %listener.local_addr(),
            max_fps = cfg.max_fps,
            bitrate_kbps = cfg.bitrate_kbps,
            codec = ?cfg.codec,
            prefer_hw = cfg.prefer_hw,
            "veloxdesk host listening"
        );
        serve_until(listener, cfg, backends, shutdown).await
    }
}

/// Start the VeloxDesk host: listen for QUIC sessions and stream capture → encode → FEC
/// datagrams while injecting remote input.
pub async fn run_host(cfg: HostConfig, id: DeviceIdentity) -> Result<()> {
    run_host_with_backends(cfg, id, HostBackends::system(), std::future::pending()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use veloxdesk_core_traits::video::{ChromaFormat, ColorSpace, EncoderConfig, PixelFormat, RateControl, VideoCodec};

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
}
