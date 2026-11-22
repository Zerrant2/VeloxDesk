//! Connect screen: address field + recent connections.

use std::net::{SocketAddr, ToSocketAddrs};

use veloxdesk_protocol::{DEFAULT_DIRECT_PORT, DEFAULT_RENDEZVOUS_PORT};

use crate::settings::Settings;

/// Parse a 9-digit VeloxDesk Device ID (ignores spaces and dashes, e.g. `"123 456 789"`).
pub fn parse_device_id(input: &str) -> Option<String> {
    let cleaned: String = input.chars().filter(|c| !c.is_ascii_whitespace() && *c != '-').collect();
    if cleaned.len() == 9 && cleaned.bytes().all(|b| b.is_ascii_digit()) {
        Some(cleaned)
    } else {
        None
    }
}

/// Format a 9-digit Device ID as `123 456 789` for display; other strings pass through unchanged.
pub fn format_device_id(id: &str) -> String {
    if id.len() == 9 && id.bytes().all(|b| b.is_ascii_digit()) {
        format!("{} {} {}", &id[0..3], &id[3..6], &id[6..9])
    } else {
        id.to_string()
    }
}

/// Parse `ip`, `ip:port`, `[v6]:port`, `host[:port]` with `default_port`.
pub fn parse_host_port(input: &str, default_port: u16) -> Result<SocketAddr, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("enter an address".into());
    }
    if let Ok(a) = s.parse::<SocketAddr>() {
        return Ok(a);
    }
    if let Ok(ip) = s.trim_matches(['[', ']']).parse::<std::net::IpAddr>() {
        return Ok(SocketAddr::new(ip, default_port));
    }
    let with_port = if s.rsplit_once(':').is_some_and(|(_, p)| p.parse::<u16>().is_ok()) {
        s.to_string()
    } else {
        format!("{s}:{default_port}")
    };
    with_port
        .to_socket_addrs()
        .map_err(|e| format!("cannot resolve {s}: {e}"))?
        .next()
        .ok_or_else(|| format!("no address for {s}"))
}

/// Parse `ip`, `ip:port`, `[v6]:port`, `host[:port]` (default port 21118).
pub fn parse_address(input: &str) -> Result<SocketAddr, String> {
    parse_host_port(input, DEFAULT_DIRECT_PORT)
}

/// Parse a rendezvous server address (default port 21116).
pub fn parse_rendezvous_addr(input: &str) -> Result<SocketAddr, String> {
    parse_host_port(input, DEFAULT_RENDEZVOUS_PORT)
}

/// Target selected by the user: either a 9-digit Device ID (P2P / relay) or a direct socket address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectTarget {
    Direct(SocketAddr),
    DeviceId(String),
}

/// Parse either a 9-digit Device ID (with optional spaces/dashes) OR a direct `ip[:port]` / `host[:port]`.
pub fn parse_target(input: &str) -> Result<ConnectTarget, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("enter a 9-digit Device ID or address".into());
    }
    if let Some(id) = parse_device_id(s) {
        return Ok(ConnectTarget::DeviceId(id));
    }
    parse_address(s).map(ConnectTarget::Direct)
}

/// What the connect screen asks the app to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectAction {
    Connect { address: String, target: ConnectTarget },
    OpenSettings,
}

#[derive(Debug, Default)]
pub struct ConnectScreen {
    pub address: String,
    /// Shown under the field (last connection error).
    pub error: Option<String>,
    /// Status text while connecting (`None` = idle).
    pub status: Option<String>,
    /// This device's ID (informational).
    pub device_id: String,
}

impl ConnectScreen {
    pub fn new(settings: &Settings) -> Self {
        Self { address: settings.recent.first().cloned().unwrap_or_default(), ..Default::default() }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, settings: &mut Settings) -> Option<ConnectAction> {
        let mut action = None;
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.heading(egui::RichText::new("VeloxDesk").size(32.0).strong());
                ui.label(format!("v{}", env!("CARGO_PKG_VERSION")));
                if !self.device_id.is_empty() {
                    ui.label(format!("This device: {}", format_device_id(&self.device_id)));
                }
                if !settings.rendezvous_server.trim().is_empty() {
                    ui.weak(format!("Rendezvous: {}", settings.rendezvous_server.trim()));
                }
                ui.add_space(16.0);
                ui.label("Remote Device ID or address (ip:port)");
                let busy = self.status.is_some();
                let edit = ui.add_enabled(
                    !busy,
                    egui::TextEdit::singleline(&mut self.address)
                        .hint_text("123 456 789 or 192.168.1.20:21118")
                        .desired_width(280.0),
                );
                let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.horizontal(|ui| {
                    ui.add_space((ui.available_width() - 260.0).max(0.0) / 2.0);
                    let connect = ui.add_enabled(!busy, egui::Button::new("Connect").min_size(egui::vec2(120.0, 28.0)));
                    if (connect.clicked() || enter) && !busy {
                        action = self.try_connect();
                    }
                    if ui.add(egui::Button::new("Settings").min_size(egui::vec2(120.0, 28.0))).clicked() {
                        action = Some(ConnectAction::OpenSettings);
                    }
                });
                ui.horizontal(|ui| {
                    ui.add_space((ui.available_width() - 260.0).max(0.0) / 2.0);
                    ui.label("Mode:");
                    ui.selectable_value(&mut settings.mode, crate::settings::Mode::Desktop, "Desktop");
                    ui.selectable_value(&mut settings.mode, crate::settings::Mode::Gaming, "Gaming");
                });
                if let Some(s) = &self.status {
                    ui.horizontal(|ui| {
                        ui.add_space((ui.available_width() - 200.0).max(0.0) / 2.0);
                        ui.spinner();
                        ui.label(s);
                    });
                }
                if let Some(e) = &self.error {
                    ui.colored_label(egui::Color32::from_rgb(255, 110, 110), e);
                }
                ui.add_space(20.0);
                if !settings.recent.is_empty() {
                    ui.label(egui::RichText::new("Recent").strong());
                    let mut remove = None;
                    for (i, addr) in settings.recent.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.add_space((ui.available_width() - 260.0).max(0.0) / 2.0);
                            if ui.add_enabled(!busy, egui::Button::new(addr).min_size(egui::vec2(220.0, 22.0))).clicked() {
                                self.address = addr.clone();
                                action = self.try_connect();
                            }
                            if ui.small_button("✕").on_hover_text("Remove").clicked() {
                                remove = Some(i);
                            }
                        });
                    }
                    if let Some(i) = remove {
                        settings.recent.remove(i);
                        settings.save_default();
                    }
                }
            });
        });
        action
    }

    fn try_connect(&mut self) -> Option<ConnectAction> {
        match parse_target(&self.address) {
            Ok(target) => {
                self.error = None;
                let address = match &target {
                    ConnectTarget::DeviceId(id) => format_device_id(id),
                    ConnectTarget::Direct(_) => self.address.trim().to_string(),
                };
                Some(ConnectAction::Connect { address, target })
            }
            Err(e) => {
                self.error = Some(e);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses() {
        assert_eq!(parse_address("10.0.0.5").unwrap(), "10.0.0.5:21118".parse().unwrap());
        assert_eq!(parse_address(" 10.0.0.5:9000 ").unwrap(), "10.0.0.5:9000".parse().unwrap());
        assert_eq!(parse_address("[::1]:5").unwrap(), "[::1]:5".parse().unwrap());
        assert_eq!(parse_address("::1").unwrap(), "[::1]:21118".parse().unwrap());
        assert_eq!(parse_address("localhost:7").unwrap().port(), 7);
        assert_eq!(parse_rendezvous_addr("10.0.0.5").unwrap(), "10.0.0.5:21116".parse().unwrap());
        assert!(parse_address("").is_err());
        assert!(parse_address("no such host .invalid").is_err());
    }

    #[test]
    fn device_ids_and_targets() {
        assert_eq!(parse_device_id("123456789").as_deref(), Some("123456789"));
        assert_eq!(parse_device_id(" 123 456 789 ").as_deref(), Some("123456789"));
        assert_eq!(parse_device_id("123-456-789").as_deref(), Some("123456789"));
        assert_eq!(parse_device_id("12345678"), None);
        assert_eq!(parse_device_id("12345678a"), None);
        assert_eq!(format_device_id("123456789"), "123 456 789");

        assert_eq!(parse_target("123 456 789").unwrap(), ConnectTarget::DeviceId("123456789".into()));
        assert_eq!(
            parse_target("10.0.0.5:21118").unwrap(),
            ConnectTarget::Direct("10.0.0.5:21118".parse().unwrap())
        );
        assert!(parse_target("   ").is_err());
    }
}
