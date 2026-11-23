//! M2 authentication and authorization tests over an in-memory authenticated channel:
//! permanent Argon2id password, rotating OTP & replay rejection, trusted Ed25519 PublicKey
//! whitelist, interactive ApprovalHandler with granular Permissions & remember_peer, and
//! TLS certificate ↔ Hello identity spoofing rejection.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use tokio::sync::{Mutex as AsyncMutex, mpsc};
use veloxdesk_core_traits::input::{DisplayGeometry, InputCaps, InputEvent, InputEventKind, InputFeedback};
use veloxdesk_core_traits::transport::{RecvStream, SendStream, TransportStats};
use veloxdesk_core_traits::video::*;
use veloxdesk_core_traits::{CoreError, FrameCapture, InputInjector, Result, Timestamp, TransportChannel, VideoEncoder};
use veloxdesk_crypto::DeviceIdentity;
use veloxdesk_crypto::auth::{
    Argon2Params, PasswordHash, derive_password_key, password_proof, sign_pubkey_challenge,
};
use veloxdesk_protocol::StreamKind;
use veloxdesk_protocol::control::{
    AuthMethod, AuthResponse, Close, Hello, KeyEvent, Permissions, envelope::Msg,
};
use veloxdesk_service::stream::{EnvelopeReader, EnvelopeWriter};
use veloxdesk_service::{
    ApprovalDecision, HostAuthConfig, HostAuthState, HostBackends, HostConfig, SimpleRate, TrustedPeer, VideoSink,
    run_session,
};

// ───────────────────── in-memory authenticated transport ─────────────────────

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

// ───────────────────── minimal synthetic backends ─────────────────────

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
            keyframe: opts.force_keyframe,
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

struct NullSink;
impl VideoSink for NullSink {
    fn send_frame(&mut self, _pkt: &EncodedPacket) -> Result<u32> {
        Ok(1)
    }
    fn set_parity_percent(&mut self, _pct: u8) {}
    fn on_nack(&mut self, _nack: &veloxdesk_protocol::control::Nack) {}
}

struct CountingInjector(Arc<AtomicUsize>);
impl InputInjector for CountingInjector {
    fn caps(&self) -> InputCaps {
        InputCaps { backend: "counting", keyboard: true, ..Default::default() }
    }
    fn set_displays(&mut self, _d: &[DisplayGeometry]) {}
    fn inject(&mut self, _e: &InputEvent) -> Result<()> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    fn release_all(&mut self) -> Result<()> {
        Ok(())
    }
    fn poll_feedback(&mut self) -> Vec<InputFeedback> {
        Vec::new()
    }
}

fn test_backends(injected: Arc<AtomicUsize>) -> HostBackends {
    HostBackends {
        capture: Arc::new(|_| Ok(Box::new(StubCapture) as Box<dyn FrameCapture>)),
        encoder: Arc::new(|cfg, _| Ok(Box::new(StubEncoder(cfg.clone(), 0)) as Box<dyn VideoEncoder>)),
        injector: Arc::new(move || Ok(Box::new(CountingInjector(injected.clone())) as Box<dyn InputInjector>)),
        video_sink: Arc::new(|_, _| Box::new(NullSink)),
        rate: Arc::new(|s, mn, mx| Box::new(SimpleRate::new(s, mn, mx))),
        audio_capture: None,
        audio_encoder: None,
        audio_playback: None,
        clipboard: None,
        privacy: None,
        file_transfer: None,
        tunnel_policy: None,
    }
}

fn fast_params() -> Argon2Params {
    Argon2Params { m_cost_kib: 1024, t_cost: 1, p_cost: 1 }
}

fn viewer_hello(id: &DeviceIdentity) -> Hello {
    Hello {
        protocol_version: 1,
        device_id: id.device_id(),
        public_key: id.public_key().to_vec(),
        device_name: "viewer-test".into(),
        os: "windows".into(),
        ..Default::default()
    }
}

async fn recv_msg(r: &mut EnvelopeReader) -> Msg {
    tokio::time::timeout(Duration::from_secs(5), r.recv())
        .await
        .expect("recv timeout")
        .expect("recv ok")
        .msg
        .expect("non-empty msg")
}

// ───────────────────────────── tests ─────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn permanent_password_accepts_valid_and_rejects_wrong() {
    let host_id = DeviceIdentity::generate();
    let viewer_id = DeviceIdentity::generate();
    let ph = PasswordHash::hash_with("correct-horse-battery", fast_params()).unwrap();

    let mut auth = HostAuthConfig::with_permanent_password(ph);
    auth.host_public_key = Some(host_id.public_key());
    let cfg = Arc::new(HostConfig { auth, ..Default::default() });

    // 1. Valid password succeeds.
    {
        let (hch, vch) = mem_pair(Some(viewer_id.public_key()));
        let host = tokio::spawn(run_session(hch, cfg.clone(), test_backends(Arc::new(AtomicUsize::new(0)))));
        let (s, r) = vch.open_bi(StreamKind::Control).await.unwrap();
        let (mut w, mut r) = (EnvelopeWriter::new(s), EnvelopeReader::new(r));
        w.send(&Msg::Hello(viewer_hello(&viewer_id)).into()).await.unwrap();

        let Msg::AuthChallenge(ch) = recv_msg(&mut r).await else { panic!("expected AuthChallenge") };
        assert_eq!(ch.methods, vec![AuthMethod::PermanentPassword as i32]);
        assert_eq!(ch.host_public_key, host_id.public_key());
        assert_eq!(ch.host_device_id, host_id.device_id());

        let params = Argon2Params {
            m_cost_kib: ch.argon2_m_cost_kib,
            t_cost: ch.argon2_t_cost,
            p_cost: ch.argon2_p_cost,
        };
        let key = derive_password_key("correct-horse-battery", &ch.salt, params).unwrap();
        let proof = password_proof(&key, &ch.nonce);
        w.send(&Msg::AuthResponse(AuthResponse { method: AuthMethod::PermanentPassword as i32, proof: proof.to_vec() }).into())
            .await
            .unwrap();

        let Msg::AuthResult(res) = recv_msg(&mut r).await else { panic!("expected AuthResult") };
        assert!(res.ok, "expected ok, got: {}", res.reason);
        let Msg::SessionConfig(_) = recv_msg(&mut r).await else { panic!("expected SessionConfig") };
        w.send(&Msg::Close(Close::default()).into()).await.unwrap();
        host.await.unwrap().unwrap();
    }

    // 2. Wrong password is rejected.
    {
        let (hch, vch) = mem_pair(Some(viewer_id.public_key()));
        let host = tokio::spawn(run_session(hch, cfg.clone(), test_backends(Arc::new(AtomicUsize::new(0)))));
        let (s, r) = vch.open_bi(StreamKind::Control).await.unwrap();
        let (mut w, mut r) = (EnvelopeWriter::new(s), EnvelopeReader::new(r));
        w.send(&Msg::Hello(viewer_hello(&viewer_id)).into()).await.unwrap();

        let Msg::AuthChallenge(ch) = recv_msg(&mut r).await else { panic!("expected AuthChallenge") };
        let params = Argon2Params {
            m_cost_kib: ch.argon2_m_cost_kib,
            t_cost: ch.argon2_t_cost,
            p_cost: ch.argon2_p_cost,
        };
        let key = derive_password_key("wrong-password", &ch.salt, params).unwrap();
        let proof = password_proof(&key, &ch.nonce);
        w.send(&Msg::AuthResponse(AuthResponse { method: AuthMethod::PermanentPassword as i32, proof: proof.to_vec() }).into())
            .await
            .unwrap();

        let Msg::AuthResult(res) = recv_msg(&mut r).await else { panic!("expected AuthResult") };
        assert!(!res.ok);
        assert!(host.await.unwrap().is_err());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn otp_succeeds_once_rotates_and_rejects_replay() {
    let host_id = DeviceIdentity::generate();
    let viewer_id = DeviceIdentity::generate();

    let mut auth = HostAuthConfig::with_otp("INIT9876");
    auth.host_public_key = Some(host_id.public_key());
    let state = HostAuthState::from_config(&auth);
    auth.shared_state = Some(state.clone());
    let cfg = Arc::new(HostConfig { auth, ..Default::default() });

    async fn attempt(cfg: Arc<HostConfig>, viewer_id: &DeviceIdentity, otp: &str) -> bool {
        let (hch, vch) = mem_pair(Some(viewer_id.public_key()));
        let host = tokio::spawn(run_session(hch, cfg, test_backends(Arc::new(AtomicUsize::new(0)))));
        let (s, r) = vch.open_bi(StreamKind::Control).await.unwrap();
        let (mut w, mut r) = (EnvelopeWriter::new(s), EnvelopeReader::new(r));
        w.send(&Msg::Hello(viewer_hello(viewer_id)).into()).await.unwrap();
        let Msg::AuthChallenge(ch) = recv_msg(&mut r).await else { panic!("expected AuthChallenge") };
        assert!(ch.methods.contains(&(AuthMethod::OneTimePassword as i32)));
        let params = Argon2Params {
            m_cost_kib: ch.argon2_m_cost_kib,
            t_cost: ch.argon2_t_cost,
            p_cost: ch.argon2_p_cost,
        };
        let key = derive_password_key(otp, &ch.salt, params).unwrap();
        let proof = password_proof(&key, &ch.nonce);
        w.send(&Msg::AuthResponse(AuthResponse { method: AuthMethod::OneTimePassword as i32, proof: proof.to_vec() }).into())
            .await
            .unwrap();
        let Msg::AuthResult(res) = recv_msg(&mut r).await else { panic!("expected AuthResult") };
        if res.ok {
            let Msg::SessionConfig(_) = recv_msg(&mut r).await else { panic!("expected SessionConfig") };
            w.send(&Msg::Close(Close::default()).into()).await.unwrap();
        }
        let _ = host.await.unwrap();
        res.ok
    }

    // 1. Initial OTP succeeds and rotates immediately.
    assert!(attempt(cfg.clone(), &viewer_id, "INIT9876").await);
    let rotated = state.current_otp().expect("rotated OTP exists");
    assert_ne!(rotated, "INIT9876");

    // 2. Replaying old OTP is rejected.
    assert!(!attempt(cfg.clone(), &viewer_id, "INIT9876").await);

    // 3. New rotated OTP succeeds.
    assert!(attempt(cfg.clone(), &viewer_id, &rotated).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn trusted_pubkey_and_approval_with_granular_permissions() {
    let host_id = DeviceIdentity::generate();
    let viewer_id = DeviceIdentity::generate();
    let approval_calls = Arc::new(AtomicUsize::new(0));
    let injected = Arc::new(AtomicUsize::new(0));

    let calls_cl = approval_calls.clone();
    let handler: veloxdesk_service::ApprovalHandler = Arc::new(move |req| {
        calls_cl.fetch_add(1, Ordering::Relaxed);
        let view_only = Permissions { view: true, input: false, ..Default::default() };
        Box::pin(async move {
            assert_eq!(req.peer_name, "viewer-test");
            ApprovalDecision::accept_and_trust(view_only)
        })
    });

    let mut auth = HostAuthConfig::require_accept_with(handler);
    auth.host_public_key = Some(host_id.public_key());
    let state = HostAuthState::from_config(&auth);
    auth.shared_state = Some(state.clone());
    let cfg = Arc::new(HostConfig { auth, ..Default::default() });

    // 1. First connection: interactive approval grants view-only + remembers peer; input is blocked.
    {
        let (hch, vch) = mem_pair(Some(viewer_id.public_key()));
        let host = tokio::spawn(run_session(hch, cfg.clone(), test_backends(injected.clone())));
        let (s, r) = vch.open_bi(StreamKind::Control).await.unwrap();
        let (mut w, mut r) = (EnvelopeWriter::new(s), EnvelopeReader::new(r));
        w.send(&Msg::Hello(viewer_hello(&viewer_id)).into()).await.unwrap();
        let Msg::AuthChallenge(ch) = recv_msg(&mut r).await else { panic!("expected AuthChallenge") };
        assert_eq!(ch.methods, vec![AuthMethod::None as i32]);
        w.send(&Msg::AuthResponse(AuthResponse { method: AuthMethod::None as i32, proof: Vec::new() }).into())
            .await
            .unwrap();
        let Msg::AuthResult(res) = recv_msg(&mut r).await else { panic!("expected AuthResult") };
        assert!(res.ok);
        let perms = res.permissions.unwrap();
        assert!(perms.view && !perms.input);
        let Msg::SessionConfig(_) = recv_msg(&mut r).await else { panic!("expected SessionConfig") };

        // Attempt inline input AND input stream: both must be ignored (0 injected).
        let key_ev = InputEvent {
            ts_us: 1,
            event: Some(InputEventKind::Key(KeyEvent { hid_usage: 0x04, down: true, modifiers: 0 })),
        };
        w.send(&Msg::Input(key_ev.clone()).into()).await.unwrap();
        let (is, _ir) = vch.open_bi(StreamKind::Input).await.unwrap();
        let mut iw = EnvelopeWriter::new(is);
        iw.send(&Msg::Input(key_ev).into()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(60)).await;
        w.send(&Msg::Close(Close::default()).into()).await.unwrap();
        let summary = host.await.unwrap().unwrap();
        assert_eq!(summary.input_events, 0);
        assert_eq!(injected.load(Ordering::Relaxed), 0);
    }
    assert_eq!(approval_calls.load(Ordering::Relaxed), 1);
    assert_eq!(state.trusted_peers().len(), 1);

    // 2. Second connection from same viewer: offered PublicKey and succeeds WITHOUT approval callback!
    {
        let (hch, vch) = mem_pair(Some(viewer_id.public_key()));
        let host = tokio::spawn(run_session(hch, cfg.clone(), test_backends(injected.clone())));
        let (s, r) = vch.open_bi(StreamKind::Control).await.unwrap();
        let (mut w, mut r) = (EnvelopeWriter::new(s), EnvelopeReader::new(r));
        w.send(&Msg::Hello(viewer_hello(&viewer_id)).into()).await.unwrap();
        let Msg::AuthChallenge(ch) = recv_msg(&mut r).await else { panic!("expected AuthChallenge") };
        assert!(ch.methods.contains(&(AuthMethod::PublicKey as i32)));
        let host_pk: [u8; 32] = ch.host_public_key.as_slice().try_into().unwrap();
        let sig = sign_pubkey_challenge(&viewer_id, &ch.nonce, &host_pk);
        w.send(&Msg::AuthResponse(AuthResponse { method: AuthMethod::PublicKey as i32, proof: sig.to_vec() }).into())
            .await
            .unwrap();
        let Msg::AuthResult(res) = recv_msg(&mut r).await else { panic!("expected AuthResult") };
        assert!(res.ok);
        let Msg::SessionConfig(_) = recv_msg(&mut r).await else { panic!("expected SessionConfig") };
        w.send(&Msg::Close(Close::default()).into()).await.unwrap();
        host.await.unwrap().unwrap();
    }
    assert_eq!(approval_calls.load(Ordering::Relaxed), 1, "trusted peer must bypass approval handler");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_spoofed_hello_identity_against_tls_key() {
    let real_viewer = DeviceIdentity::generate();
    let spoofed_viewer = DeviceIdentity::generate();
    let mut auth = HostAuthConfig::with_trusted_peers(vec![TrustedPeer::new(spoofed_viewer.public_key(), "spoof")]);
    auth.host_public_key = Some(DeviceIdentity::generate().public_key());
    let cfg = Arc::new(HostConfig { auth, ..Default::default() });

    let (hch, vch) = mem_pair(Some(real_viewer.public_key()));
    let host = tokio::spawn(run_session(hch, cfg, test_backends(Arc::new(AtomicUsize::new(0)))));
    let (s, r) = vch.open_bi(StreamKind::Control).await.unwrap();
    let (mut w, mut r) = (EnvelopeWriter::new(s), EnvelopeReader::new(r));
    // Claim to be spoofed_viewer while TLS cert is real_viewer.
    w.send(&Msg::Hello(viewer_hello(&spoofed_viewer)).into()).await.unwrap();
    let Msg::AuthResult(res) = recv_msg(&mut r).await else { panic!("expected AuthResult") };
    assert!(!res.ok);
    assert!(res.reason.contains("TLS"));
    assert!(host.await.unwrap().is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_password_mode_switches_and_security_guard_and_peer_revocation() {
    let host_id = DeviceIdentity::generate();
    let viewer_id = DeviceIdentity::generate();
    let injected = Arc::new(AtomicUsize::new(0));

    let mut auth = HostAuthConfig::with_otp("OTP11111");
    auth.host_public_key = Some(host_id.public_key());
    let state = HostAuthState::from_config(&auth);
    auth.shared_state = Some(state.clone());
    let cfg = Arc::new(HostConfig { auth, ..Default::default() });

    let params = Argon2Params { m_cost_kib: 1024, t_cost: 1, p_cost: 1 };
    let perm_hash = PasswordHash::hash_with("PermSecret!99", params).unwrap();

    // 1. Live switch to Permanent-only (OTP disabled, permanent hash set).
    state.set_permanent_password_hash(Some(perm_hash));
    assert_eq!(state.set_otp_enabled(false), None);

    // 1a. Permanent-only offers ONLY PermanentPassword (not OneTimePassword, not None) and works repeatedly.
    for _ in 0..2 {
        let (hch, vch) = mem_pair(Some(viewer_id.public_key()));
        let host = tokio::spawn(run_session(hch, cfg.clone(), test_backends(injected.clone())));
        let (s, r) = vch.open_bi(StreamKind::Control).await.unwrap();
        let (mut w, mut r) = (EnvelopeWriter::new(s), EnvelopeReader::new(r));
        w.send(&Msg::Hello(viewer_hello(&viewer_id)).into()).await.unwrap();
        let Msg::AuthChallenge(ch) = recv_msg(&mut r).await else { panic!("expected AuthChallenge") };
        assert_eq!(ch.methods, vec![AuthMethod::PermanentPassword as i32]);
        let key = derive_password_key("PermSecret!99", &ch.salt, params).unwrap();
        let proof = password_proof(&key, &ch.nonce);
        w.send(&Msg::AuthResponse(AuthResponse { method: AuthMethod::PermanentPassword as i32, proof: proof.to_vec() }).into())
            .await
            .unwrap();
        let Msg::AuthResult(res) = recv_msg(&mut r).await else { panic!("expected AuthResult") };
        assert!(res.ok, "permanent password must succeed repeatedly");
        let Msg::SessionConfig(_) = recv_msg(&mut r).await else { panic!("expected SessionConfig") };
        w.send(&Msg::Close(Close::default()).into()).await.unwrap();
        host.await.unwrap().unwrap();
    }

    // 1b. Permanent-only rejects an OneTimePassword attempt.
    {
        let (hch, vch) = mem_pair(Some(viewer_id.public_key()));
        let host = tokio::spawn(run_session(hch, cfg.clone(), test_backends(injected.clone())));
        let (s, r) = vch.open_bi(StreamKind::Control).await.unwrap();
        let (mut w, mut r) = (EnvelopeWriter::new(s), EnvelopeReader::new(r));
        w.send(&Msg::Hello(viewer_hello(&viewer_id)).into()).await.unwrap();
        let Msg::AuthChallenge(_) = recv_msg(&mut r).await else { panic!("expected AuthChallenge") };
        w.send(&Msg::AuthResponse(AuthResponse { method: AuthMethod::OneTimePassword as i32, proof: vec![0u8; 32] }).into())
            .await
            .unwrap();
        let Msg::AuthResult(res) = recv_msg(&mut r).await else { panic!("expected AuthResult") };
        assert!(!res.ok);
        assert!(host.await.unwrap().is_err());
    }

    // 2. Live switch to Both (OTP enabled + permanent hash set): both work, OTP rotates on use.
    let otp = state.set_otp_enabled(true).expect("OTP enabled");
    {
        let (hch, vch) = mem_pair(Some(viewer_id.public_key()));
        let host = tokio::spawn(run_session(hch, cfg.clone(), test_backends(injected.clone())));
        let (s, r) = vch.open_bi(StreamKind::Control).await.unwrap();
        let (mut w, mut r) = (EnvelopeWriter::new(s), EnvelopeReader::new(r));
        w.send(&Msg::Hello(viewer_hello(&viewer_id)).into()).await.unwrap();
        let Msg::AuthChallenge(ch) = recv_msg(&mut r).await else { panic!("expected AuthChallenge") };
        assert!(ch.methods.contains(&(AuthMethod::PermanentPassword as i32)));
        assert!(ch.methods.contains(&(AuthMethod::OneTimePassword as i32)));
        assert!(!ch.methods.contains(&(AuthMethod::None as i32)));
        let key = derive_password_key(&otp, &ch.salt, params).unwrap();
        let proof = password_proof(&key, &ch.nonce);
        w.send(&Msg::AuthResponse(AuthResponse { method: AuthMethod::PermanentPassword as i32, proof: proof.to_vec() }).into())
            .await
            .unwrap();
        let Msg::AuthResult(res) = recv_msg(&mut r).await else { panic!("expected AuthResult") };
        assert!(res.ok, "OTP should succeed via single-prompt PermanentPassword path when Both enabled");
        let Msg::SessionConfig(_) = recv_msg(&mut r).await else { panic!("expected SessionConfig") };
        w.send(&Msg::Close(Close::default()).into()).await.unwrap();
        host.await.unwrap().unwrap();
    }
    assert_ne!(state.current_otp().as_deref(), Some(otp.as_str()), "OTP must rotate on use in Both mode");

    // 3. Security guard: clearing BOTH OTP and permanent password with allow_no_auth=false
    //    must NEVER offer AuthMethod::None and must reject untrusted peers immediately.
    state.set_permanent_password_hash(None);
    state.set_otp_enabled(false);
    {
        let (hch, vch) = mem_pair(Some(viewer_id.public_key()));
        let host = tokio::spawn(run_session(hch, cfg.clone(), test_backends(injected.clone())));
        let (s, r) = vch.open_bi(StreamKind::Control).await.unwrap();
        let (mut w, mut r) = (EnvelopeWriter::new(s), EnvelopeReader::new(r));
        w.send(&Msg::Hello(viewer_hello(&viewer_id)).into()).await.unwrap();
        let Msg::AuthResult(res) = recv_msg(&mut r).await else { panic!("expected immediate AuthResult rejection") };
        assert!(!res.ok);
        assert!(host.await.unwrap().is_err());
    }

    // 4. Live trusted peer addition & immediate revocation via remove_trusted_peer.
    state.add_trusted_peer(TrustedPeer::new(viewer_id.public_key(), "viewer-1"));
    {
        let (hch, vch) = mem_pair(Some(viewer_id.public_key()));
        let host = tokio::spawn(run_session(hch, cfg.clone(), test_backends(injected.clone())));
        let (s, r) = vch.open_bi(StreamKind::Control).await.unwrap();
        let (mut w, mut r) = (EnvelopeWriter::new(s), EnvelopeReader::new(r));
        w.send(&Msg::Hello(viewer_hello(&viewer_id)).into()).await.unwrap();
        let Msg::AuthChallenge(ch) = recv_msg(&mut r).await else { panic!("expected AuthChallenge") };
        assert_eq!(ch.methods, vec![AuthMethod::PublicKey as i32]);
        let host_pk: [u8; 32] = ch.host_public_key.as_slice().try_into().unwrap();
        let sig = sign_pubkey_challenge(&viewer_id, &ch.nonce, &host_pk);
        w.send(&Msg::AuthResponse(AuthResponse { method: AuthMethod::PublicKey as i32, proof: sig.to_vec() }).into())
            .await
            .unwrap();
        let Msg::AuthResult(res) = recv_msg(&mut r).await else { panic!("expected AuthResult") };
        assert!(res.ok);
        let Msg::SessionConfig(_) = recv_msg(&mut r).await else { panic!("expected SessionConfig") };
        w.send(&Msg::Close(Close::default()).into()).await.unwrap();
        host.await.unwrap().unwrap();
    }

    assert!(state.remove_trusted_peer(&viewer_id.public_key()));
    assert!(!state.remove_trusted_peer(&viewer_id.public_key()));
    {
        let (hch, vch) = mem_pair(Some(viewer_id.public_key()));
        let host = tokio::spawn(run_session(hch, cfg, test_backends(injected)));
        let (s, r) = vch.open_bi(StreamKind::Control).await.unwrap();
        let (mut w, mut r) = (EnvelopeWriter::new(s), EnvelopeReader::new(r));
        w.send(&Msg::Hello(viewer_hello(&viewer_id)).into()).await.unwrap();
        let Msg::AuthResult(res) = recv_msg(&mut r).await else { panic!("expected immediate AuthResult rejection after revocation") };
        assert!(!res.ok);
        assert!(host.await.unwrap().is_err());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_default_permissions_override_applies_to_open_password_and_approval() {
    let host_id = DeviceIdentity::generate();
    let viewer_id = DeviceIdentity::generate();
    let injected = Arc::new(AtomicUsize::new(0));

    // 1. Open mode honors live HostAuthState::set_default_permissions.
    let open_state = HostAuthState::from_config(&HostAuthConfig::open());
    let custom_open = Permissions {
        view: true,
        input: false,
        clipboard: false,
        file_transfer: false,
        audio: true,
        tunnel: false,
        terminal: false,
        restart: false,
        privacy_mode: false,
        allow_chat: true,
        allow_power: true,
    };
    open_state.set_default_permissions(custom_open.clone());
    let mut open_auth = HostAuthConfig::open();
    open_auth.shared_state = Some(open_state);
    let open_cfg = Arc::new(HostConfig { auth: open_auth, audio_enabled: false, ..Default::default() });
    {
        let (hch, vch) = mem_pair(Some(viewer_id.public_key()));
        let host = tokio::spawn(run_session(hch, open_cfg, test_backends(injected.clone())));
        let (s, r) = vch.open_bi(StreamKind::Control).await.unwrap();
        let (mut w, mut r) = (EnvelopeWriter::new(s), EnvelopeReader::new(r));
        w.send(&Msg::Hello(viewer_hello(&viewer_id)).into()).await.unwrap();
        let Msg::AuthResult(res) = recv_msg(&mut r).await else { panic!("expected AuthResult") };
        assert!(res.ok);
        assert_eq!(res.permissions.as_ref(), Some(&custom_open));
        let Msg::SessionConfig(_) = recv_msg(&mut r).await else { panic!("expected SessionConfig") };
        w.send(&Msg::Close(Close::default()).into()).await.unwrap();
        host.await.unwrap().unwrap();
    }

    // 2. Permanent password login honors live override AND passes it as ApprovalRequest.default_permissions.
    let hash = PasswordHash::hash_with("v060-secret", Argon2Params { m_cost_kib: 1024, t_cost: 1, p_cost: 1 }).unwrap();
    let mut pw_auth = HostAuthConfig::with_permanent_password(hash);
    pw_auth.host_public_key = Some(host_id.public_key());
    let pw_state = HostAuthState::from_config(&pw_auth);
    pw_auth.shared_state = Some(pw_state.clone());

    let seen_default = Arc::new(std::sync::Mutex::new(None::<Permissions>));
    let seen_c = seen_default.clone();
    pw_auth.require_accept = true;
    pw_auth.approval_handler = Some(Arc::new(move |req| {
        let seen_c = seen_c.clone();
        Box::pin(async move {
            *seen_c.lock().unwrap() = Some(req.default_permissions.clone());
            ApprovalDecision::accept(req.default_permissions)
        })
    }));

    let live_perms = Permissions {
        view: true,
        input: true,
        clipboard: false,
        file_transfer: true,
        audio: false,
        tunnel: false,
        terminal: true,
        restart: false,
        privacy_mode: true,
        allow_chat: false,
        allow_power: true,
    };
    pw_state.set_default_permissions(live_perms.clone());
    let pw_cfg = Arc::new(HostConfig { auth: pw_auth, ..Default::default() });
    {
        let (hch, vch) = mem_pair(Some(viewer_id.public_key()));
        let host = tokio::spawn(run_session(hch, pw_cfg, test_backends(injected)));
        let (s, r) = vch.open_bi(StreamKind::Control).await.unwrap();
        let (mut w, mut r) = (EnvelopeWriter::new(s), EnvelopeReader::new(r));
        w.send(&Msg::Hello(viewer_hello(&viewer_id)).into()).await.unwrap();
        let Msg::AuthChallenge(ch) = recv_msg(&mut r).await else { panic!("expected AuthChallenge") };
        let key = derive_password_key(
            "v060-secret",
            &ch.salt,
            Argon2Params { m_cost_kib: ch.argon2_m_cost_kib, t_cost: ch.argon2_t_cost, p_cost: ch.argon2_p_cost },
        )
        .unwrap();
        let proof = password_proof(&key, &ch.nonce);
        w.send(&Msg::AuthResponse(AuthResponse { method: AuthMethod::PermanentPassword as i32, proof: proof.to_vec() }).into())
            .await
            .unwrap();
        let Msg::AuthResult(res) = recv_msg(&mut r).await else { panic!("expected AuthResult") };
        assert!(res.ok);
        assert_eq!(res.permissions.as_ref(), Some(&live_perms));
        assert_eq!(seen_default.lock().unwrap().as_ref(), Some(&live_perms));
        let Msg::SessionConfig(_) = recv_msg(&mut r).await else { panic!("expected SessionConfig") };
        w.send(&Msg::Close(Close::default()).into()).await.unwrap();
        host.await.unwrap().unwrap();
    }
}
