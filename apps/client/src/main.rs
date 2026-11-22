//! VeloxDesk client and host application.

fn main() {
    println!("veloxdesk {} (protocol v{})", env!("CARGO_PKG_VERSION"), veloxdesk_protocol::PROTOCOL_VERSION);
}
