//! VeloxDesk self-hosted rendezvous + zero-knowledge relay server (`veloxdesk-server`).

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use tokio::sync::watch;
use tracing_subscriber::EnvFilter;

use veloxdesk_server_relay::{RelayConfig, RelayServer};
use veloxdesk_server_rendezvous::{RendezvousConfig, RendezvousServer};

#[derive(Debug, Parser)]
#[command(
    name = "veloxdesk-server",
    version,
    about = "VeloxDesk self-hosted rendezvous (ID registry + NAT punch) and zero-knowledge relay server"
)]
struct Args {
    /// UDP address to bind the rendezvous server.
    #[arg(long, default_value = "0.0.0.0:21116")]
    rendezvous_bind: SocketAddr,

    /// UDP address to bind the zero-knowledge relay server.
    #[arg(long, default_value = "0.0.0.0:21117")]
    relay_bind: SocketAddr,

    /// Public UDP address advertised to peers for relay fallback (defaults to --relay-bind).
    #[arg(long)]
    public_relay_addr: Option<SocketAddr>,

    /// Peer registration TTL in seconds.
    #[arg(long, default_value_t = 90)]
    peer_ttl: u64,

    /// Keepalive interval advertised to registered peers in seconds.
    #[arg(long, default_value_t = 30)]
    keepalive_secs: u32,

    /// Idle relay session TTL in seconds.
    #[arg(long, default_value_t = 120)]
    relay_session_ttl: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let args = Args::parse();
    let advertised_relay = args.public_relay_addr.unwrap_or(args.relay_bind);

    let relay = RelayServer::bind(RelayConfig {
        bind: args.relay_bind,
        session_ttl: Duration::from_secs(args.relay_session_ttl),
    })
    .await?;

    let rendezvous = RendezvousServer::bind(RendezvousConfig {
        bind: args.rendezvous_bind,
        relay_addr: Some(advertised_relay),
        peer_ttl: Duration::from_secs(args.peer_ttl),
        keepalive_secs: args.keepalive_secs,
    })
    .await?;

    tracing::info!(
        rendezvous = %rendezvous.local_addr(),
        relay = %relay.local_addr(),
        public_relay = %advertised_relay,
        peer_ttl_s = args.peer_ttl,
        "veloxdesk-server ready (Ctrl+C to stop)"
    );

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let rdv_rx = shutdown_rx.clone();
    let relay_rx = shutdown_rx;

    let rdv_task = tokio::spawn(async move { rendezvous.run_until(rdv_rx).await });
    let relay_task = tokio::spawn(async move { relay.run_until(relay_rx).await });

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("received Ctrl+C, shutting down");
            let _ = shutdown_tx.send(true);
        }
        res = rdv_task => {
            let _ = shutdown_tx.send(true);
            res??;
            return Ok(());
        }
        res = relay_task => {
            let _ = shutdown_tx.send(true);
            res??;
            return Ok(());
        }
    }

    Ok(())
}
