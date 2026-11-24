//! Integration tests for v0.5.0 host session features:
//! - Bidirectional chat (`ChatMessage` / `ChatAck` / `HostStatusEvent::ChatReceived` / `ChatAcked`)
//! - `allow_chat = false` permission denial
//! - `SystemPowerRequest` denied when `allow_power = false`
//! - `SystemPowerRequest` executed via `SimulatedPowerExecutor` when `allow_power = true`,
//!   verifying `SystemPowerResult { ok: true }` arrives BEFORE execution!

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use tokio::sync::{Mutex as AsyncMutex, broadcast, mpsc, watch};
use veloxdesk_core_traits::input::{DisplayGeometry, InputCaps, InputEvent, InputFeedback};
use veloxdesk_core_traits::transport::{RecvStream, SendStream, TransportStats};
use veloxdesk_core_traits::video::*;
use veloxdesk_core_traits::{CoreError, FrameCapture, InputInjector, Result, Timestamp, TransportChannel, VideoEncoder};
use veloxdesk_crypto::DeviceIdentity;
use veloxdesk_crypto::auth::sign_pubkey_challenge;
use veloxdesk_protocol::StreamKind;
use veloxdesk_protocol::control::{
    AuthMethod, AuthResponse, ChatAck, ChatMessage, Close, Envelope, Hello, Permissions, Ping, SystemPowerAction,
    SystemPowerRequest, envelope::Msg,
};
use veloxdesk_service::stream::{EnvelopeReader, EnvelopeWriter};
use veloxdesk_service::{
    ApprovalDecision, HostAuthConfig, HostBackends, HostConfig, HostStatusEvent, RecordedPowerAction, SessionHooks,
    SimpleRate, SimulatedPowerExecutor, TrustedPeer, VideoSink, run_session_with_hooks,
};

type Incoming = (StreamKind, SendStream, RecvStream);

struct MemChannel {
    peer_tx: mpsc::UnboundedSender<Incoming>,
    incoming: AsyncMutex<mpsc::UnboundedReceiver<Incoming>>,
    peer_pk: Option<[u8; 32]>,
}

fn mem_pair(host_sees_peer_pk: Option<[u8; 32]>) -> (Arc<MemChannel>, Arc<MemChannel>) {
    let (a_tx, a_rx) = mpsc::unbounded_channel();
    let (b_tx, b_rx) = mpsc::unbounded_channel();
    let host = Arc::new(MemChannel { peer_tx: b_tx, incoming: AsyncMutex::new(a_rx), peer_pk: host_sees_peer_pk });
    let viewer = Arc::new(MemChannel { peer_tx: a_tx, incoming: AsyncMutex::new(b_rx), peer_pk: None });
    (host, viewer)
}

#[async_trait]
impl TransportChannel for MemChannel {
    fn max_datagram_size(&self) -> Option<usize> {
        Some(1200)
    }
    fn send_datagram(&self, _data: Bytes) -> Result<()> {
        Ok(())
    }
    async fn recv_datagram(&self) -> Result<Bytes> {
        std::future::pending().await
    }
    async fn open_bi(&self, kind: StreamKind) -> Result<(SendStream, RecvStream)> {
        let (local, remote) = tokio::io::duplex(1 << 16);
        let (lr, lw) = tokio::io::split(local);
        let (rr, rw) = tokio::io::split(remote);
        self.peer_tx.send((kind, Box::pin(rw), Box::pin(rr))).map_err(|_| CoreError::Closed)?;
        Ok((Box::pin(lw) as SendStream, Box::pin(lr) as RecvStream))
    }
    async fn open_uni(&self, _kind: StreamKind) -> Result<SendStream> {
        Err(CoreError::unsupported("uni"))
    }
    async fn accept_bi(&self) -> Result<(StreamKind, SendStream, RecvStream)> {
        self.incoming.lock().await.recv().await.ok_or(CoreError::Closed)
    }
    async fn accept_uni(&self) -> Result<(StreamKind, RecvStream)> {
        std::future::pending().await
    }
    fn stats(&self) -> TransportStats {
        TransportStats::default()
    }
    fn peer_public_key(&self) -> Option<[u8; 32]> {
        self.peer_pk
    }
    fn close(&self, _code: u32, _reason: &str) {}
}

struct StubCapture;
impl FrameCapture for StubCapture {
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
    fn next_frame(&mut self, _timeout: Duration) -> Result<Option<CapturedFrame>> {
        std::thread::sleep(Duration::from_millis(5));
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
            dirty_rects: vec![],
            cursor: None,
            capture_us: 10,
            convert_us: 0,
        }))
    }
    fn stop(&mut self) {}
}

struct StubEncoder(EncoderConfig, u32);
impl VideoEncoder for StubEncoder {
    fn caps(&self) -> EncoderCaps {
        EncoderCaps { name: "stub", codecs: vec![VideoCodec::H264], ..Default::default() }
    }
    fn config(&self) -> &EncoderConfig {
        &self.0
    }
    fn encode(&mut self, frame: &Frame, opts: EncodeOptions) -> Result<Option<EncodedPacket>> {
        self.1 += 1;
        Ok(Some(EncodedPacket {
            data: Bytes::from_static(&[0, 0, 0, 1, 0x65]),
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

struct StubInjector;
impl InputInjector for StubInjector {
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

struct NullSink;
impl VideoSink for NullSink {
    fn send_frame(&mut self, _pkt: &EncodedPacket) -> Result<u32> {
        Ok(0)
    }
    fn set_parity_percent(&mut self, _pct: u8) {}
    fn on_nack(&mut self, _nack: &veloxdesk_protocol::control::Nack) {}
}

fn stub_backends() -> HostBackends {
    HostBackends {
        capture: Arc::new(|_| Ok(Box::new(StubCapture))),
        encoder: Arc::new(|cfg, _| Ok(Box::new(StubEncoder(cfg.clone(), 0)))),
        injector: Arc::new(|| Ok(Box::new(StubInjector))),
        video_sink: Arc::new(|_, _| Box::new(NullSink)),
        rate: Arc::new(|s, mi, ma| Box::new(SimpleRate::new(s, mi, ma))),
        audio_capture: None,
        audio_encoder: None,
        audio_playback: None,
        mic_playback: None,
        clipboard: None,
        privacy: None,
        file_transfer: None,
        tunnel_policy: None,
    }
}

async fn wait_for_msg<F>(r: &mut EnvelopeReader, mut pred: F) -> Envelope
where
    F: FnMut(&Envelope) -> bool,
{
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let env = r.recv().await.expect("control stream closed unexpectedly");
            if pred(&env) {
                return env;
            }
        }
    })
    .await
    .expect("timed out waiting for expected control message")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_bidirectional_and_allow_chat_denied() {
    // Part A: allow_chat = true (default open session) — Viewer <-> Host chat + acks.
    {
        let (host_ch, viewer_ch) = mem_pair(None);
        let events = Arc::new(Mutex::new(Vec::<HostStatusEvent>::new()));
        let events_c = events.clone();
        let (chat_tx, _) = broadcast::channel::<ChatMessage>(16);
        let (_stop_tx, stop_rx) = watch::channel(false);

        let hooks = SessionHooks {
            active_sessions: None,
            on_event: Some(Arc::new(move |ev| events_c.lock().unwrap().push(ev))),
            chat_tx: Some(chat_tx.clone()),
            power_executor: None,
            ..Default::default()
        };
        let cfg = Arc::new(HostConfig { audio_enabled: false, ..Default::default() });
        let sess = tokio::spawn(run_session_with_hooks(host_ch, cfg, stub_backends(), stop_rx, hooks));

        let (send, recv) = viewer_ch.open_bi(StreamKind::Control).await.unwrap();
        let mut w = EnvelopeWriter::new(send);
        let mut r = EnvelopeReader::new(recv);

        w.send(&Msg::Hello(Hello { device_id: "111222333".into(), ..Default::default() }).into()).await.unwrap();
        let auth_env = wait_for_msg(&mut r, |e| matches!(e.msg, Some(Msg::AuthResult(_)))).await;
        let Some(Msg::AuthResult(ar)) = auth_env.msg else { unreachable!() };
        assert!(ar.ok);
        assert!(ar.permissions.as_ref().unwrap().allow_chat);
        assert!(!ar.permissions.as_ref().unwrap().allow_power);
        let _ = wait_for_msg(&mut r, |e| matches!(e.msg, Some(Msg::SessionConfig(_)))).await;

        // Viewer -> Host chat.
        w.send(
            &Msg::ChatMessage(ChatMessage {
                id: 10,
                sender_name: "Alice".into(),
                text: "Hi host".into(),
                ts_ms: 123456,
            })
            .into(),
        )
        .await
        .unwrap();
        let ack_env = wait_for_msg(&mut r, |e| matches!(e.msg, Some(Msg::ChatAck(_)))).await;
        assert!(matches!(ack_env.msg, Some(Msg::ChatAck(ChatAck { id: 10 }))));

        // Host -> Viewer chat via SessionHooks::chat_tx.
        chat_tx
            .send(ChatMessage { id: 99, sender_name: "Host".into(), text: "Hello Alice".into(), ts_ms: 999999 })
            .unwrap();
        let host_chat = wait_for_msg(&mut r, |e| matches!(e.msg, Some(Msg::ChatMessage(_)))).await;
        assert!(matches!(
            host_chat.msg,
            Some(Msg::ChatMessage(ChatMessage { id: 99, ref text, .. })) if text == "Hello Alice"
        ));

        // Viewer acks host chat.
        w.send(&Msg::ChatAck(ChatAck { id: 99 }).into()).await.unwrap();
        w.send(&Msg::Ping(Ping { seq: 1, sent_us: 1 }).into()).await.unwrap();
        let _ = wait_for_msg(&mut r, |e| matches!(e.msg, Some(Msg::Pong(_)))).await;

        w.send(&Msg::Close(Close { code: 0, reason: "done".into() }).into()).await.unwrap();
        let _ = sess.await.unwrap().unwrap();

        let evs = events.lock().unwrap();
        assert!(evs.iter().any(|e| matches!(
            e,
            HostStatusEvent::ChatReceived { msg, .. } if msg.id == 10 && msg.text == "Hi host"
        )));
        assert!(evs.iter().any(|e| matches!(e, HostStatusEvent::ChatAcked { id: 99, .. })));
    }

    // Part B: allow_chat = false — viewer chat is ignored (no ChatReceived, no ChatAck) and host chat is suppressed.
    {
        let (host_ch, viewer_ch) = mem_pair(None);
        let events = Arc::new(Mutex::new(Vec::<HostStatusEvent>::new()));
        let events_c = events.clone();
        let (chat_tx, _) = broadcast::channel::<ChatMessage>(16);
        let (_stop_tx, stop_rx) = watch::channel(false);

        let approval = Arc::new(|mut req: veloxdesk_service::ApprovalRequest| {
            Box::pin(async move {
                req.default_permissions.allow_chat = false;
                ApprovalDecision::accept(req.default_permissions)
            }) as std::pin::Pin<Box<dyn std::future::Future<Output = ApprovalDecision> + Send>>
        });

        let cfg = Arc::new(HostConfig {
            audio_enabled: false,
            auth: HostAuthConfig::require_accept_with(approval),
            ..Default::default()
        });
        let hooks = SessionHooks {
            active_sessions: None,
            on_event: Some(Arc::new(move |ev| events_c.lock().unwrap().push(ev))),
            chat_tx: Some(chat_tx.clone()),
            power_executor: None,
            ..Default::default()
        };
        let sess = tokio::spawn(run_session_with_hooks(host_ch, cfg, stub_backends(), stop_rx, hooks));

        let (send, recv) = viewer_ch.open_bi(StreamKind::Control).await.unwrap();
        let mut w = EnvelopeWriter::new(send);
        let mut r = EnvelopeReader::new(recv);

        w.send(&Msg::Hello(Hello { device_id: "222333444".into(), ..Default::default() }).into()).await.unwrap();
        let _ = wait_for_msg(&mut r, |e| matches!(e.msg, Some(Msg::AuthChallenge(_)))).await;
        w.send(&Msg::AuthResponse(AuthResponse { method: AuthMethod::None as i32, proof: Vec::new() }).into())
            .await
            .unwrap();
        let auth_env = wait_for_msg(&mut r, |e| matches!(e.msg, Some(Msg::AuthResult(_)))).await;
        let Some(Msg::AuthResult(ar)) = auth_env.msg else { unreachable!() };
        assert!(ar.ok);
        assert!(!ar.permissions.as_ref().unwrap().allow_chat);
        let _ = wait_for_msg(&mut r, |e| matches!(e.msg, Some(Msg::SessionConfig(_)))).await;

        // Send viewer chat + ping barrier: Pong must arrive without any ChatAck.
        w.send(
            &Msg::ChatMessage(ChatMessage {
                id: 55,
                sender_name: "Blocked".into(),
                text: "Should be ignored".into(),
                ts_ms: 1,
            })
            .into(),
        )
        .await
        .unwrap();
        let _ = chat_tx.send(ChatMessage { id: 56, sender_name: "Host".into(), text: "No".into(), ts_ms: 2 });
        w.send(&Msg::Ping(Ping { seq: 42, sent_us: 1 }).into()).await.unwrap();

        loop {
            let env = tokio::time::timeout(Duration::from_secs(5), r.recv()).await.unwrap().unwrap();
            match env.msg {
                Some(Msg::ChatAck(_)) => panic!("ChatAck must not be sent when allow_chat = false"),
                Some(Msg::ChatMessage(_)) => panic!("ChatMessage must not be sent when allow_chat = false"),
                Some(Msg::Pong(p)) if p.seq == 42 => break,
                _ => {}
            }
        }

        w.send(&Msg::Close(Close { code: 0, reason: "done".into() }).into()).await.unwrap();
        let _ = sess.await.unwrap().unwrap();

        let evs = events.lock().unwrap();
        assert!(!evs.iter().any(|e| matches!(e, HostStatusEvent::ChatReceived { .. })));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn system_power_request_denied_by_default_and_executed_when_allowed() {
    // Part A: default open session has allow_power = false -> denied cleanly, executor not called.
    {
        let (host_ch, viewer_ch) = mem_pair(None);
        let sim = Arc::new(SimulatedPowerExecutor::new());
        let (_stop_tx, stop_rx) = watch::channel(false);
        let hooks = SessionHooks {
            active_sessions: None,
            on_event: None,
            chat_tx: None,
            power_executor: Some(sim.clone()),
            ..Default::default()
        };
        let cfg = Arc::new(HostConfig { audio_enabled: false, ..Default::default() });
        let sess = tokio::spawn(run_session_with_hooks(host_ch, cfg, stub_backends(), stop_rx, hooks));

        let (send, recv) = viewer_ch.open_bi(StreamKind::Control).await.unwrap();
        let mut w = EnvelopeWriter::new(send);
        let mut r = EnvelopeReader::new(recv);

        w.send(&Msg::Hello(Hello { device_id: "333444555".into(), ..Default::default() }).into()).await.unwrap();
        let _ = wait_for_msg(&mut r, |e| matches!(e.msg, Some(Msg::SessionConfig(_)))).await;

        w.send(
            &Msg::SystemPowerRequest(SystemPowerRequest {
                action: SystemPowerAction::Reboot as i32,
                reason: "unauthorized".into(),
                delay_secs: 5,
            })
            .into(),
        )
        .await
        .unwrap();

        let res_env = wait_for_msg(&mut r, |e| matches!(e.msg, Some(Msg::SystemPowerResult(_)))).await;
        let Some(Msg::SystemPowerResult(res)) = res_env.msg else { unreachable!() };
        assert!(!res.ok);
        assert_eq!(res.action, SystemPowerAction::Reboot as i32);
        assert!(res.error.contains("allow_power"));
        assert!(sim.calls().is_empty(), "power executor must not be invoked when denied");

        w.send(&Msg::Close(Close { code: 0, reason: "done".into() }).into()).await.unwrap();
        let _ = sess.await.unwrap().unwrap();
    }

    // Part B: TrustedPeer with Permissions::all() (allow_power = true) -> SystemPowerResult { ok: true }
    // arrives BEFORE SimulatedPowerExecutor::execute runs!
    {
        let host_id = DeviceIdentity::generate();
        let viewer_id = DeviceIdentity::generate();
        let (host_ch, viewer_ch) = mem_pair(Some(viewer_id.public_key()));
        let sim = Arc::new(SimulatedPowerExecutor::new());
        let (_stop_tx, stop_rx) = watch::channel(false);

        let mut auth = HostAuthConfig::with_trusted_peers(vec![TrustedPeer::new(
            viewer_id.public_key(),
            "admin-workstation",
        )]);
        auth.host_public_key = Some(host_id.public_key());

        let cfg = Arc::new(HostConfig { audio_enabled: false, auth, ..Default::default() });
        let hooks = SessionHooks {
            active_sessions: None,
            on_event: None,
            chat_tx: None,
            power_executor: Some(sim.clone()),
            ..Default::default()
        };
        let sess = tokio::spawn(run_session_with_hooks(host_ch, cfg, stub_backends(), stop_rx, hooks));

        let (send, recv) = viewer_ch.open_bi(StreamKind::Control).await.unwrap();
        let mut w = EnvelopeWriter::new(send);
        let mut r = EnvelopeReader::new(recv);

        w.send(
            &Msg::Hello(Hello {
                device_id: viewer_id.device_id(),
                public_key: viewer_id.public_key().to_vec(),
                device_name: "admin-workstation".into(),
                ..Default::default()
            })
            .into(),
        )
        .await
        .unwrap();

        let ch_env = wait_for_msg(&mut r, |e| matches!(e.msg, Some(Msg::AuthChallenge(_)))).await;
        let Some(Msg::AuthChallenge(challenge)) = ch_env.msg else { unreachable!() };
        let sig = sign_pubkey_challenge(&viewer_id, &challenge.nonce, &host_id.public_key());
        w.send(&Msg::AuthResponse(AuthResponse { method: AuthMethod::PublicKey as i32, proof: sig.to_vec() }).into())
            .await
            .unwrap();

        let auth_env = wait_for_msg(&mut r, |e| matches!(e.msg, Some(Msg::AuthResult(_)))).await;
        let Some(Msg::AuthResult(ar)) = auth_env.msg else { unreachable!() };
        assert!(ar.ok);
        assert_eq!(ar.permissions.as_ref().unwrap(), &Permissions::all());
        let _ = wait_for_msg(&mut r, |e| matches!(e.msg, Some(Msg::SessionConfig(_)))).await;

        w.send(
            &Msg::SystemPowerRequest(SystemPowerRequest {
                action: SystemPowerAction::Reboot as i32,
                reason: "Windows Update".into(),
                delay_secs: 5,
            })
            .into(),
        )
        .await
        .unwrap();

        let res_env = wait_for_msg(&mut r, |e| matches!(e.msg, Some(Msg::SystemPowerResult(_)))).await;
        let Some(Msg::SystemPowerResult(res)) = res_env.msg else { unreachable!() };
        assert!(res.ok, "expected ok=true, got err={}", res.error);
        assert_eq!(res.action, SystemPowerAction::Reboot as i32);
        assert!(res.error.is_empty());

        // Ack must arrive BEFORE the 50ms flush delay + OS power execution!
        assert!(
            sim.calls().is_empty(),
            "SystemPowerResult {{ ok: true }} must arrive before PowerExecutor::execute runs"
        );

        // Wait for execution after the ~50ms grace yield.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while sim.calls().is_empty() && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            sim.calls(),
            vec![RecordedPowerAction {
                action: SystemPowerAction::Reboot,
                reason: "Windows Update".into(),
                delay_secs: 5,
            }]
        );

        w.send(&Msg::Close(Close { code: 0, reason: "done".into() }).into()).await.unwrap();
        let _ = sess.await.unwrap().unwrap();
    }
}
