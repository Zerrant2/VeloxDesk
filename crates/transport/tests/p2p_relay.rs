//! Real loopback UDP/QUIC integration tests for M2 P2P + Zero-Knowledge Relay:
//! * Direct LAN `connect()` into `P2pListener` (`PathKind::Direct`)
//! * Connect-by-ID via `RendezvousServer` + `P2pListener` + `connect_p2p` (`PathKind::P2p`)
//! * Forced zero-knowledge relay (`relay_only: true`, `PathKind::Relay`) + FEC video frames
//! * Mutual-TLS Ed25519 `expected_peer` rejection (both P2P and Relay)
//! * Concurrent Direct + Relay sessions on one multiplexed `P2pListener` UDP socket
//!
//! NOTE: requires working UDP loopback (run unsandboxed).

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::watch;

use veloxdesk_core_traits::transport::PathKind;
use veloxdesk_core_traits::video::{EncodedPacket, VideoCodec};
use veloxdesk_core_traits::{Timestamp, TransportChannel};
use veloxdesk_crypto::DeviceIdentity;
use veloxdesk_protocol::StreamKind;
use veloxdesk_protocol::control::{Ping, Pong, envelope::Msg};
use veloxdesk_server_relay::{RelayConfig, RelayServer};
use veloxdesk_server_rendezvous::{RendezvousConfig, RendezvousServer};
use veloxdesk_transport::{
    P2pConnectOptions, P2pListener, VideoReceiver, VideoSender, connect, connect_p2p, framed,
};

async fn spawn_servers() -> (std::net::SocketAddr, std::net::SocketAddr, watch::Sender<bool>) {
    let relay = RelayServer::bind(RelayConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        session_ttl: Duration::from_secs(60),
    })
    .await
    .unwrap();
    let relay_addr = relay.local_addr();

    let rdv = RendezvousServer::bind(RendezvousConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        relay_addr: Some(relay_addr),
        peer_ttl: Duration::from_secs(60),
        keepalive_secs: 15,
    })
    .await
    .unwrap();
    let rdv_addr = rdv.local_addr();

    let (tx, rx) = watch::channel(false);
    tokio::spawn(relay.run_until(rx.clone()));
    tokio::spawn(rdv.run_until(rx));
    (rdv_addr, relay_addr, tx)
}

async fn verify_control_and_video(
    host: &veloxdesk_transport::QuicChannel,
    viewer: &veloxdesk_transport::QuicChannel,
) {
    // Control stream roundtrip.
    let (vs, vr) = viewer.open_bi(StreamKind::Control).await.unwrap();
    let (mut vtx, mut vrx) = framed(vs, vr);
    vtx.send(&Msg::Ping(Ping { seq: 11, sent_us: 999 }).into()).await.unwrap();

    let (kind, hs, hr) = host.accept_bi().await.unwrap();
    assert_eq!(kind, StreamKind::Control);
    let (mut htx, mut hrx) = framed(hs, hr);
    assert_eq!(hrx.recv().await.unwrap(), Msg::Ping(Ping { seq: 11, sent_us: 999 }).into());
    htx.send(&Msg::Pong(Pong { seq: 11, ping_sent_us: 999, peer_recv_us: 1000, peer_send_us: 1001 }).into())
        .await
        .unwrap();
    match vrx.recv().await.unwrap().msg {
        Some(Msg::Pong(p)) => assert_eq!(p.seq, 11),
        other => panic!("expected Pong, got {other:?}"),
    }

    // Video FEC datagram roundtrip (keyframe 64 KB + P-frame 16 KB).
    let mut sender = VideoSender::new(Arc::new(host.clone()), 0);
    sender.set_parity_percent(20);
    let mut receiver = VideoReceiver::new();
    receiver.set_host_time_reference(Timestamp::now());

    for (id, len, key) in [(0u32, 64_000usize, true), (1u32, 16_000usize, false)] {
        let data: Bytes = (0..len).map(|i| ((i.wrapping_mul(31) ^ id as usize) & 0xff) as u8).collect::<Vec<u8>>().into();
        let pkt = EncodedPacket {
            data: data.clone(),
            codec: VideoCodec::H264,
            keyframe: key,
            recovery_point: false,
            width: 1920,
            height: 1080,
            capture_ts: Timestamp::now(),
            frame_id: id,
            encode_us: 100,
            convert_us: 50,
        };
        sender.send_frame(&pkt).unwrap();

        let got = loop {
            let dg = viewer.recv_datagram().await.unwrap();
            if let Some(f) = receiver.push_datagram(&dg, Timestamp::now()) {
                break f;
            }
        };
        assert_eq!(got.packet.frame_id, id);
        assert_eq!(got.packet.data, data);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn p2p_direct_and_relay_multiplexed_on_one_socket() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let (rdv_addr, _relay_addr, shutdown) = spawn_servers().await;
        let host_id = DeviceIdentity::generate();
        let listener = Arc::new(
            P2pListener::bind("127.0.0.1:0".parse().unwrap(), Some(rdv_addr), &host_id)
                .await
                .unwrap(),
        );
        let host_addr = listener.local_addr();

        // 1. P2P connect by 9-digit Device ID (with spaces to verify normalization).
        let viewer1_id = DeviceIdentity::generate();
        let spaced_id = format!(
            "{} {} {}",
            &host_id.device_id()[0..3],
            &host_id.device_id()[3..6],
            &host_id.device_id()[6..9]
        );
        let accept1 = {
            let l = listener.clone();
            tokio::spawn(async move { l.accept().await.unwrap() })
        };
        let viewer1 = connect_p2p(P2pConnectOptions::new(rdv_addr, spaced_id), &viewer1_id)
            .await
            .unwrap();
        let host1 = accept1.await.unwrap();
        assert_eq!(viewer1.stats().path, PathKind::P2p);
        assert_eq!(host1.stats().path, PathKind::P2p);
        assert_eq!(viewer1.peer_public_key(), Some(host_id.public_key()));
        assert_eq!(host1.peer_public_key(), Some(viewer1_id.public_key()));
        verify_control_and_video(&host1, &viewer1).await;

        // 2. Forced zero-knowledge Relay on the SAME P2pListener UDP socket.
        let viewer2_id = DeviceIdentity::generate();
        let accept2 = {
            let l = listener.clone();
            tokio::spawn(async move { l.accept().await.unwrap() })
        };
        let mut relay_opts = P2pConnectOptions::new(rdv_addr, host_id.device_id());
        relay_opts.relay_only = true;
        let viewer2 = connect_p2p(relay_opts, &viewer2_id).await.unwrap();
        let host2 = accept2.await.unwrap();
        assert_eq!(viewer2.stats().path, PathKind::Relay);
        assert_eq!(host2.stats().path, PathKind::Relay);
        assert_eq!(viewer2.peer_public_key(), Some(host_id.public_key()));
        assert_eq!(host2.peer_public_key(), Some(viewer2_id.public_key()));
        verify_control_and_video(&host2, &viewer2).await;

        // 3. Direct LAN IP connect() on the SAME P2pListener UDP socket.
        let viewer3_id = DeviceIdentity::generate();
        let accept3 = {
            let l = listener.clone();
            tokio::spawn(async move { l.accept().await.unwrap() })
        };
        let viewer3 = connect(host_addr, &viewer3_id, Some(host_id.public_key())).await.unwrap();
        let host3 = accept3.await.unwrap();
        assert_eq!(viewer3.stats().path, PathKind::Direct);
        assert_eq!(host3.stats().path, PathKind::Direct);
        verify_control_and_video(&host3, &viewer3).await;

        listener.shutdown();
        let _ = shutdown.send(true);
    })
    .await
    .expect("p2p/relay test timed out (UDP loopback blocked?)");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn expected_peer_mismatch_rejected_over_p2p_and_relay() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let (rdv_addr, _relay_addr, shutdown) = spawn_servers().await;
        let host_id = DeviceIdentity::generate();
        let viewer_id = DeviceIdentity::generate();
        let listener = Arc::new(
            P2pListener::bind("127.0.0.1:0".parse().unwrap(), Some(rdv_addr), &host_id)
                .await
                .unwrap(),
        );
        let bg = {
            let l = listener.clone();
            tokio::spawn(async move {
                let _ = l.accept().await;
            })
        };

        // P2P with wrong expected_peer is rejected by mutual-TLS immediately.
        let mut p2p_bad = P2pConnectOptions::new(rdv_addr, host_id.device_id());
        p2p_bad.expected_peer = Some([0xAB; 32]);
        assert!(connect_p2p(p2p_bad, &viewer_id).await.is_err());

        // Relay with wrong expected_peer is rejected by mutual-TLS (proves E2EE through relay).
        let mut relay_bad = P2pConnectOptions::new(rdv_addr, host_id.device_id());
        relay_bad.relay_only = true;
        relay_bad.expected_peer = Some([0xCD; 32]);
        assert!(connect_p2p(relay_bad, &viewer_id).await.is_err());

        listener.shutdown();
        bg.abort();
        let _ = shutdown.send(true);
    })
    .await
    .expect("expected_peer mismatch test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unspecified_relay_addr_resolves_to_rendezvous_ip() {
    tokio::time::timeout(Duration::from_secs(20), async {
        // Bind relay on 127.0.0.1:<ephemeral>, but configure RendezvousServer to advertise
        // 0.0.0.0:<ephemeral> (matching default `veloxdesk-server --relay-bind 0.0.0.0:21117`).
        let relay = RelayServer::bind(RelayConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            session_ttl: Duration::from_secs(60),
        })
        .await
        .unwrap();
        let relay_port = relay.local_addr().port();
        let advertised_unspecified: std::net::SocketAddr = ([0, 0, 0, 0], relay_port).into();

        let rdv = RendezvousServer::bind(RendezvousConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            relay_addr: Some(advertised_unspecified),
            peer_ttl: Duration::from_secs(60),
            keepalive_secs: 15,
        })
        .await
        .unwrap();
        let rdv_addr = rdv.local_addr();

        let (tx, rx) = watch::channel(false);
        tokio::spawn(relay.run_until(rx.clone()));
        tokio::spawn(rdv.run_until(rx));

        let host_id = DeviceIdentity::generate();
        let viewer_id = DeviceIdentity::generate();
        let listener = Arc::new(
            P2pListener::bind("127.0.0.1:0".parse().unwrap(), Some(rdv_addr), &host_id)
                .await
                .unwrap(),
        );
        let accept = {
            let l = listener.clone();
            tokio::spawn(async move { l.accept().await.unwrap() })
        };

        let mut opts = P2pConnectOptions::new(rdv_addr, host_id.device_id());
        opts.relay_only = true;
        let viewer = connect_p2p(opts, &viewer_id).await.unwrap();
        let host = accept.await.unwrap();
        assert_eq!(viewer.stats().path, PathKind::Relay);
        assert_eq!(host.stats().path, PathKind::Relay);
        verify_control_and_video(&host, &viewer).await;

        listener.shutdown();
        let _ = tx.send(true);
    })
    .await
    .expect("unspecified relay addr test timed out");
}
