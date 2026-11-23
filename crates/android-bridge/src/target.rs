//! Target address & 9-digit Device ID parsing (compatible with VeloxDesk desktop viewer).

use std::net::{IpAddr, SocketAddr, ToSocketAddrs};

use veloxdesk_protocol::{DEFAULT_DIRECT_PORT, DEFAULT_RENDEZVOUS_PORT};

/// Parsed viewer connection target: either a 9-digit Device ID (P2P / relay via rendezvous)
/// or a direct socket address (`ip`, `ip:port`, `[ipv6]:port`, `host[:port]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectTarget {
    Direct(SocketAddr),
    DeviceId(String),
}

/// Parse a 9-digit VeloxDesk Device ID (ignores ASCII whitespace and `-`, e.g. `"123 456 789"`).
pub fn parse_device_id(input: &str) -> Option<String> {
    let cleaned: String = input.chars().filter(|c| !c.is_ascii_whitespace() && *c != '-').collect();
    if cleaned.len() == 9 && cleaned.bytes().all(|b| b.is_ascii_digit()) {
        Some(cleaned)
    } else {
        None
    }
}

/// Format a 9-digit Device ID as `"123 456 789"`; other strings pass through unchanged.
pub fn format_device_id(id: &str) -> String {
    if id.len() == 9 && id.bytes().all(|b| b.is_ascii_digit()) {
        format!("{} {} {}", &id[0..3], &id[3..6], &id[6..9])
    } else {
        id.to_string()
    }
}

/// Parse `ip`, `ip:port`, `[ipv6]:port`, or `host[:port]` using `default_port` when omitted.
pub fn parse_host_port(input: &str, default_port: u16) -> Result<SocketAddr, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("address is empty".into());
    }
    if let Ok(addr) = s.parse::<SocketAddr>() {
        return Ok(addr);
    }
    if let Ok(ip) = s.trim_matches(['[', ']']).parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, default_port));
    }
    let with_port = if s.rsplit_once(':').is_some_and(|(_, p)| p.parse::<u16>().is_ok()) {
        s.to_string()
    } else {
        format!("{s}:{default_port}")
    };
    with_port
        .to_socket_addrs()
        .map_err(|e| format!("cannot resolve '{s}': {e}"))?
        .next()
        .ok_or_else(|| format!("no address resolved for '{s}'"))
}

/// Parse a direct host address (default port 21118).
pub fn parse_direct_addr(input: &str) -> Result<SocketAddr, String> {
    parse_host_port(input, DEFAULT_DIRECT_PORT)
}

/// Parse a rendezvous server address (default port 21116).
pub fn parse_rendezvous_addr(input: &str) -> Result<SocketAddr, String> {
    parse_host_port(input, DEFAULT_RENDEZVOUS_PORT)
}

/// Parse either a 9-digit Device ID (with optional spaces/dashes) or a direct `ip[:port]` / `host[:port]`.
pub fn parse_target(input: &str) -> Result<ConnectTarget, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("target address or Device ID is empty".into());
    }
    if let Some(id) = parse_device_id(s) {
        return Ok(ConnectTarget::DeviceId(id));
    }
    parse_direct_addr(s).map(ConnectTarget::Direct)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_device_ids_and_formats() {
        assert_eq!(parse_device_id("123456789").as_deref(), Some("123456789"));
        assert_eq!(parse_device_id(" 123 456 789 ").as_deref(), Some("123456789"));
        assert_eq!(parse_device_id("123-456-789").as_deref(), Some("123456789"));
        assert_eq!(parse_device_id("12345678"), None);
        assert_eq!(parse_device_id("1234567890"), None);
        assert_eq!(parse_device_id("12345678a"), None);
        assert_eq!(format_device_id("123456789"), "123 456 789");
        assert_eq!(format_device_id("10.0.0.5"), "10.0.0.5");
    }

    #[test]
    fn parses_ipv4_ipv6_and_rendezvous() {
        assert_eq!(
            parse_target("192.168.1.42").unwrap(),
            ConnectTarget::Direct("192.168.1.42:21118".parse().unwrap())
        );
        assert_eq!(
            parse_target(" 10.0.0.5:9000 ").unwrap(),
            ConnectTarget::Direct("10.0.0.5:9000".parse().unwrap())
        );
        assert_eq!(
            parse_target("[::1]:5555").unwrap(),
            ConnectTarget::Direct("[::1]:5555".parse().unwrap())
        );
        assert_eq!(
            parse_target("::1").unwrap(),
            ConnectTarget::Direct("[::1]:21118".parse().unwrap())
        );
        assert_eq!(
            parse_target("[fe80::1]").unwrap(),
            ConnectTarget::Direct("[fe80::1]:21118".parse().unwrap())
        );
        assert_eq!(
            parse_target("123 456 789").unwrap(),
            ConnectTarget::DeviceId("123456789".into())
        );
        assert_eq!(
            parse_rendezvous_addr("10.0.0.2").unwrap(),
            "10.0.0.2:21116".parse().unwrap()
        );
        assert_eq!(
            parse_rendezvous_addr("[::1]:31116").unwrap(),
            "[::1]:31116".parse().unwrap()
        );
        assert!(parse_target("   ").is_err());
        assert!(parse_rendezvous_addr("").is_err());
    }
}
