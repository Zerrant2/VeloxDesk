//! VeloxDesk self-hosted rendezvous + relay server.

fn main() {
    println!("veloxdesk-server {} (protocol v{})", env!("CARGO_PKG_VERSION"), veloxdesk_protocol::PROTOCOL_VERSION);
}
