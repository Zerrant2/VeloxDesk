//! End-to-end host session over an in-memory transport with synthetic capture/encoder,
//! a recording video sink and a recording injector. Exercises: handshake, SessionConfig,
//! FrameTiming, Ping/Pong, PLI → keyframe, ReceiverReport → BitrateUpdate + parity,
//! Input stream → injector, release_all on disconnect, clean shutdown.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use tokio::sync::{Mutex as AsyncMutex, mpsc};
use veloxdesk_core_traits::input::{DisplayGeometry, InputCaps, InputEvent, InputEventKind, InputFeedback};
use veloxdesk_core_traits::transport::{RecvStream, SendStream, TransportStats};
use veloxdesk_core_traits::video::*;
use veloxdesk_core_traits::{CoreError, FrameCapture, InputInjector, Result, Timestamp, TransportChannel, VideoEncoder};
use veloxdesk_protocol::StreamKind;
use veloxdesk_protocol::control::{
    Envelope, Hello, KeyEvent, KeyframeRequest, Nack, Ping, ReceiverReport, envelope::Msg,
};
use veloxdesk_service::stream::{EnvelopeReader, EnvelopeWriter};
use veloxdesk_service::{HostBackends, HostConfig, SimpleRate, VideoSink, run_session};

// ───────────── in-memory transport ─────────────

type Incoming = (StreamKind, SendStream, RecvStream);

struct MemChannel {
    peer_tx: mpsc::UnboundedSender<Incoming>,
    incoming: AsyncMutex<mpsc::UnboundedReceiver<Incoming>>,
    closed: Mutex<Option<(u32, String)>>,
}

fn mem_pair() -> (Arc<MemChannel>, Arc<MemChannel>) {
    let (a_tx, a_rx) = mpsc::unbounded_channel();
    let (b_tx, b_rx) = mpsc::unbounded_channel();
    let a = Arc::new(MemChannel { peer_tx: b_tx, incoming: AsyncMutex::new(a_rx), closed: Mutex::new(None) });
    let b = Arc::new(MemChannel { peer_tx: a_tx, incoming: AsyncMutex::new(b_rx), closed: Mutex::new(None) });
    (a, b)
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
        None
    }
    fn close(&self, code: u32, reason: &str) {
        *self.closed.lock().unwrap() = Some((code, reason.to_string()));
    }
}

// ───────────── synthetic backends ─────────────

struct SynthCapture {
    started: bool,
}

impl FrameCapture for SynthCapture {
    fn caps(&self) -> CaptureCaps {
        CaptureCaps { backend: "synthetic", formats: vec![PixelFormat::Bgra8], ..Default::default() }
    }
    fn displays(&self) -> Result<Vec<Display>> {
        Ok(vec![Display {
            id: DisplayId("synth0".into()),
            index: 0,
            name: "Synthetic".into(),
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
        self.started = true;
        Ok(())
    }
    fn next_frame(&mut self, _timeout: Duration) -> Result<Option<CapturedFrame>> {
        assert!(self.started);
        std::thread::sleep(Duration::from_millis(2));
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
            capture_us: 100,
            convert_us: 0,
        }))
    }
    fn stop(&mut self) {}
}

struct SynthEncoder {
    cfg: EncoderConfig,
    next_id: u32,
    bitrate: Arc<Mutex<Vec<u32>>>,
}

impl VideoEncoder for SynthEncoder {
    fn caps(&self) -> EncoderCaps {
        EncoderCaps { name: "synthetic", codecs: vec![VideoCodec::H264], ..Default::default() }
    }
    fn config(&self) -> &EncoderConfig {
        &self.cfg
    }
    fn encode(&mut self, frame: &Frame, opts: EncodeOptions) -> Result<Option<EncodedPacket>> {
        self.next_id += 1;
        Ok(Some(EncodedPacket {
            data: Bytes::from_static(&[0, 0, 0, 1, 0x65, 1, 2, 3]),
            codec: self.cfg.codec,
            keyframe: opts.force_keyframe,
            recovery_point: false,
            width: frame.width,
            height: frame.height,
            capture_ts: frame.capture_ts,
            frame_id: self.next_id,
            encode_us: 500,
            convert_us: 50,
        }))
    }
    fn update(&mut self, u: EncoderUpdate) -> Result<()> {
        if let Some(b) = u.bitrate_kbps {
            self.bitrate.lock().unwrap().push(b);
        }
        Ok(())
    }
}

#[derive(Default)]
struct SinkLog {
    frames: Vec<(u32, bool)>,
    parity: Vec<u8>,
    nacks: u32,
}

struct RecSink(Arc<Mutex<SinkLog>>);

impl VideoSink for RecSink {
    fn send_frame(&mut self, pkt: &EncodedPacket) -> Result<u32> {
        self.0.lock().unwrap().frames.push((pkt.frame_id, pkt.keyframe));
        Ok(10)
    }
    fn set_parity_percent(&mut self, pct: u8) {
        self.0.lock().unwrap().parity.push(pct);
    }
    fn on_nack(&mut self, _nack: &Nack) {
        self.0.lock().unwrap().nacks += 1;
    }
}

#[derive(Default)]
struct InjLog {
    events: Vec<InputEvent>,
    displays: Vec<DisplayGeometry>,
    releases: u32,
}

struct RecInjector(Arc<Mutex<InjLog>>);

impl InputInjector for RecInjector {
    fn caps(&self) -> InputCaps {
        InputCaps { backend: "recording", keyboard: true, ..Default::default() }
    }
    fn set_displays(&mut self, d: &[DisplayGeometry]) {
        self.0.lock().unwrap().displays = d.to_vec();
    }
    fn inject(&mut self, e: &InputEvent) -> Result<()> {
        self.0.lock().unwrap().events.push(e.clone());
        Ok(())
    }
    fn release_all(&mut self) -> Result<()> {
        self.0.lock().unwrap().releases += 1;
        Ok(())
    }
    fn poll_feedback(&mut self) -> Vec<InputFeedback> {
        Vec::new()
    }
}

async fn recv_until<F: FnMut(&Msg) -> bool>(r: &mut EnvelopeReader, mut f: F) -> Msg {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let env: Envelope = r.recv().await.expect("control recv");
            if let Some(m) = env.msg {
                if f(&m) {
                    return m;
                }
            }
        }
    })
    .await
    .expect("timed out waiting for message")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_session_over_memory_transport() {
    let sink_log = Arc::new(Mutex::new(SinkLog::default()));
    let inj_log = Arc::new(Mutex::new(InjLog::default()));
    let bitrates = Arc::new(Mutex::new(Vec::new()));
    let backends = {
        let sink_log = sink_log.clone();
        let inj_log = inj_log.clone();
        let bitrates = bitrates.clone();
        HostBackends {
            capture: Arc::new(|_gpu| Ok(Box::new(SynthCapture { started: false }) as Box<dyn FrameCapture>)),
            encoder: Arc::new(move |cfg, _hw| {
                Ok(Box::new(SynthEncoder { cfg: cfg.clone(), next_id: 0, bitrate: bitrates.clone() })
                    as Box<dyn VideoEncoder>)
            }),
            injector: Arc::new(move || Ok(Box::new(RecInjector(inj_log.clone())) as Box<dyn InputInjector>)),
            video_sink: Arc::new(move |_ch, _sid| Box::new(RecSink(sink_log.clone())) as Box<dyn VideoSink>),
            rate: Arc::new(|s, mn, mx| Box::new(SimpleRate::new(s, mn, mx)) as Box<dyn veloxdesk_service::RateController>),
            audio_capture: None,
            audio_encoder: None,
            audio_playback: None,
        }
    };
    let cfg = Arc::new(HostConfig { max_fps: 60, bitrate_kbps: 10_000, ..Default::default() });
    let (host_ch, viewer_ch) = mem_pair();
    let host = tokio::spawn(run_session(host_ch.clone() as Arc<dyn TransportChannel>, cfg, backends));

    // control stream + Hello
    let (send, recv) = viewer_ch.open_bi(StreamKind::Control).await.unwrap();
    let mut w = EnvelopeWriter::new(send);
    let mut r = EnvelopeReader::new(recv);
    w.send(&Msg::Hello(Hello { device_id: "123456789".into(), protocol_version: 1, ..Default::default() }).into())
        .await
        .unwrap();
    let Msg::AuthResult(a) = recv_until(&mut r, |_| true).await else { panic!("expected AuthResult") };
    assert!(a.ok);
    assert!(a.permissions.unwrap().input);
    let Msg::SessionConfig(sc) = recv_until(&mut r, |_| true).await else { panic!("expected SessionConfig") };
    let v = sc.video.unwrap();
    assert_eq!((v.width, v.height, v.encoder_name.as_str()), (64, 32, "synthetic"));
    assert_eq!(sc.displays.len(), 1);

    // frames flow with timing reports
    let Msg::FrameTiming(t) = recv_until(&mut r, |m| matches!(m, Msg::FrameTiming(_))).await else { unreachable!() };
    assert_eq!((t.capture_us, t.convert_us, t.encode_us, t.packetize_us), (100, 50, 500, 10));

    // ping → pong
    w.send(&Msg::Ping(Ping { seq: 7, sent_us: 42 }).into()).await.unwrap();
    let Msg::Pong(p) = recv_until(&mut r, |m| matches!(m, Msg::Pong(_))).await else { unreachable!() };
    assert_eq!((p.seq, p.ping_sent_us), (7, 42));
    assert!(p.peer_send_us >= p.peer_recv_us);

    // PLI → a keyframe after the first one
    let kf_before = sink_log.lock().unwrap().frames.iter().filter(|f| f.1).count();
    assert!(kf_before >= 1, "first frame must be a keyframe");
    w.send(&Msg::KeyframeRequest(KeyframeRequest::default()).into()).await.unwrap();
    w.send(&Msg::Nack(Nack::default()).into()).await.unwrap();

    // ABR: lossy report → bitrate down + parity up
    w.send(
        &Msg::ReceiverReport(ReceiverReport { packets_received: 80, packets_lost: 20, ..Default::default() }).into(),
    )
    .await
    .unwrap();
    let Msg::BitrateUpdate(b) = recv_until(&mut r, |m| matches!(m, Msg::BitrateUpdate(_))).await else {
        unreachable!()
    };
    assert_eq!(b.bitrate_kbps, 7_000);
    assert_eq!(b.fec_parity_percent, 40);

    // input stream
    let (isend, _irecv) = viewer_ch.open_bi(StreamKind::Input).await.unwrap();
    let mut iw = EnvelopeWriter::new(isend);
    for down in [true, false, true] {
        let ev = InputEvent {
            ts_us: 1,
            event: Some(InputEventKind::Key(KeyEvent { hid_usage: 0x04, down, modifiers: 0 })),
        };
        iw.send(&Msg::Input(ev).into()).await.unwrap();
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        while inj_log.lock().unwrap().events.len() < 3 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("input events not injected");
    tokio::time::sleep(Duration::from_millis(50)).await;

    // close
    w.send(&Msg::Close(Default::default()).into()).await.unwrap();
    let summary = tokio::time::timeout(Duration::from_secs(5), host).await.unwrap().unwrap().unwrap();
    assert!(summary.frames_sent > 3, "{summary:?}");
    assert_eq!(summary.input_events, 3);
    assert!(summary.close_reason.contains("viewer closed"));

    let sl = sink_log.lock().unwrap();
    assert!(sl.frames.iter().filter(|f| f.1).count() > kf_before, "PLI produced no keyframe");
    assert_eq!(sl.nacks, 1);
    assert_eq!(sl.parity.first(), Some(&10));
    assert!(sl.parity.contains(&40));
    assert_eq!(bitrates.lock().unwrap().as_slice(), &[7_000]);
    let il = inj_log.lock().unwrap();
    assert!(il.releases >= 1, "release_all not called on disconnect");
    assert_eq!(il.displays.len(), 1);
    assert!(host_ch.closed.lock().unwrap().is_some());

}
