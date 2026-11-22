//! Connect screen: address field + recent connections.

use std::net::{SocketAddr, ToSocketAddrs};

use veloxdesk_protocol::DEFAULT_DIRECT_PORT;

use crate::settings::Settings;

/// Parse `ip`, `ip:port`, `[v6]:port`, `host[:port]` (default port 21118).
pub fn parse_address(input: &str) -> Result<SocketAddr, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("enter an address".into());
    }
    if let Ok(a) = s.parse::<SocketAddr>() {
        return Ok(a);
    }
    if let Ok(ip) = s.trim_matches(['[', ']']).parse::<std::net::IpAddr>() {
        return Ok(SocketAddr::new(ip, DEFAULT_DIRECT_PORT));
    }
    let with_port = if s.rsplit_once(':').is_some_and(|(_, p)| p.parse::<u16>().is_ok()) {
        s.to_string()
    } else {
        format!("{s}:{DEFAULT_DIRECT_PORT}")
    };
    with_port
        .to_socket_addrs()
        .map_err(|e| format!("cannot resolve {s}: {e}"))?
        .next()
        .ok_or_else(|| format!("no address for {s}"))
}

/// What the connect screen asks the app to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectAction {
    Connect { address: String, addr: SocketAddr },
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
                    ui.label(format!("This device: {}", self.device_id));
                }
                ui.add_space(16.0);
                ui.label("Remote address (ip:port)");
                let busy = self.status.is_some();
                let edit = ui.add_enabled(
                    !busy,
                    egui::TextEdit::singleline(&mut self.address).hint_text("192.168.1.20:21118").desired_width(280.0),
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
        match parse_address(&self.address) {
            Ok(addr) => {
                self.error = None;
                Some(ConnectAction::Connect { address: self.address.trim().to_string(), addr })
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
        assert!(parse_address("").is_err());
        assert!(parse_address("no such host .invalid").is_err());
    }
}
