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

/// A discovered peer on the user's Tailscale tailnet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailscalePeer {
    pub name: String,
    pub ip: String,
    pub os: String,
    pub online: bool,
}

/// Snapshot of local Tailscale CLI status and discovered peers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TailscaleInfo {
    pub installed: bool,
    pub running: bool,
    pub self_ip: Option<String>,
    pub self_name: Option<String>,
    pub peers: Vec<TailscalePeer>,
    pub error: Option<String>,
}

/// What the connect screen asks the app to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectAction {
    Connect { address: String, target: ConnectTarget },
    OpenSettings,
    /// Start (`true`) or stop (`false`) the embedded local host.
    ToggleHost(bool),
    /// Rotate the one-time password on the embedded host.
    RotateOtp,
    /// Copy text (Device ID, OTP, or Tailscale IP) to the system clipboard.
    CopyText(String),
    /// Toggle whether incoming sessions require interactive Accept/Reject approval.
    SetRequireAccept(bool),
    /// Re-query local `tailscale status --json` in the background.
    RefreshTailscale,
    /// Launch / log in to Tailscale (`tailscale-ipn.exe` or `tailscale up`).
    StartTailscale,
    /// Open an external URL in the user's default browser.
    OpenUrl(String),
}

#[derive(Debug, Default)]
pub struct ConnectScreen {
    pub address: String,
    /// Shown under the field (last connection error).
    pub error: Option<String>,
    /// Status text while connecting (`None` = idle).
    pub status: Option<String>,
    /// This device's 9-digit ID.
    pub device_id: String,
    /// Whether the embedded local host is currently running.
    pub host_running: bool,
    /// Human-readable host status (e.g. `"Listening on :21118"`, `"Stopped"`, `"Connected: ..."`).
    pub host_status: String,
    /// Current one-time password for incoming connections.
    pub host_otp: String,
    /// Whether the OTP is shown in plaintext (`false` = masked).
    pub show_otp: bool,
    /// Number of active incoming sessions connected to this desk.
    pub active_sessions: usize,
    /// Last embedded-host error, if any.
    pub host_error: Option<String>,
    /// Transient visual feedback after copying (e.g. `"ID copied"` / `"OTP copied"`).
    pub copied_notice: Option<String>,
    /// Auto-detected Tailscale status and peers for 1-click P2P connections.
    pub tailscale: TailscaleInfo,
}

impl ConnectScreen {
    pub fn new(settings: &Settings) -> Self {
        Self {
            address: settings.recent.first().cloned().unwrap_or_default(),
            host_status: "Stopped".into(),
            ..Default::default()
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, settings: &mut Settings) -> Option<ConnectAction> {
        let mut action = None;
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(16.0);
            ui.vertical_centered(|ui| {
                ui.heading(egui::RichText::new("VeloxDesk").size(30.0).strong());
                ui.horizontal(|ui| {
                    let badge = if settings.rendezvous_server.trim().is_empty() {
                        format!("v{}", env!("CARGO_PKG_VERSION"))
                    } else {
                        format!("v{}  ·  Rendezvous: {}", env!("CARGO_PKG_VERSION"), settings.rendezvous_server.trim())
                    };
                    let width = ui.available_width();
                    ui.add_space((width - (badge.len() as f32 * 6.5)).max(0.0) / 2.0);
                    ui.weak(badge);
                });
            });
            ui.add_space(18.0);

            let max_w = ui.available_width().min(860.0);
            let side_pad = ((ui.available_width() - max_w) * 0.5).max(8.0);
            ui.horizontal(|ui| {
                ui.add_space(side_pad);
                ui.allocate_ui_with_layout(
                    egui::vec2(max_w, ui.available_height()),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.columns(2, |cols| {
                            self.show_this_desk(&mut cols[0], settings, &mut action);
                            self.show_remote_desk(&mut cols[1], settings, &mut action);
                        });
                    },
                );
            });
        });
        action
    }

    fn show_this_desk(&mut self, ui: &mut egui::Ui, settings: &mut Settings, action: &mut Option<ConnectAction>) {
        egui::Frame::group(ui.style()).inner_margin(egui::Margin::same(14)).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("This Desk").size(18.0).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (dot_color, pill) = if self.active_sessions > 0 {
                        (egui::Color32::from_rgb(100, 190, 255), format!("{} active", self.active_sessions))
                    } else if self.host_running {
                        (egui::Color32::from_rgb(110, 230, 140), "Ready".into())
                    } else {
                        (egui::Color32::from_rgb(150, 150, 160), "Offline".into())
                    };
                    ui.colored_label(dot_color, format!("● {pill}"));
                });
            });
            ui.weak("Share your Device ID and one-time password to allow remote access.");
            ui.separator();

            // Device ID block
            ui.label(egui::RichText::new("Device ID").small().weak());
            let formatted_id = if self.device_id.is_empty() {
                "Unavailable".to_string()
            } else {
                format_device_id(&self.device_id)
            };
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&formatted_id).monospace().size(22.0).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(!self.device_id.is_empty(), egui::Button::new("Copy ID"))
                        .clicked()
                    {
                        self.copied_notice = Some("Device ID copied".into());
                        *action = Some(ConnectAction::CopyText(formatted_id.clone()));
                    }
                });
            });

            if self.tailscale.running
                && let Some(ts_ip) = self.tailscale.self_ip.clone()
            {
                ui.add_space(6.0);
                ui.label(egui::RichText::new("Tailscale IP").small().weak());
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(&ts_ip).monospace().size(15.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Copy").clicked() {
                            self.copied_notice = Some("Tailscale IP copied".into());
                            *action = Some(ConnectAction::CopyText(ts_ip.clone()));
                        }
                    });
                });
            }

            ui.add_space(10.0);

            // One-Time Password block
            ui.label(egui::RichText::new("One-Time Password").small().weak());
            let otp_display = if self.host_otp.is_empty() {
                "—".to_string()
            } else if self.show_otp {
                self.host_otp.clone()
            } else {
                "•".repeat(self.host_otp.len().max(8))
            };
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&otp_display).monospace().size(18.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let has_otp = !self.host_otp.is_empty();
                    if ui
                        .add_enabled(self.host_running, egui::Button::new("Refresh"))
                        .on_hover_text("Generate a new one-time password")
                        .clicked()
                    {
                        self.copied_notice = None;
                        *action = Some(ConnectAction::RotateOtp);
                    }
                    if ui.add_enabled(has_otp, egui::Button::new("Copy")).clicked() {
                        self.copied_notice = Some("OTP copied".into());
                        *action = Some(ConnectAction::CopyText(self.host_otp.clone()));
                    }
                    if ui
                        .add_enabled(has_otp, egui::Button::new(if self.show_otp { "Hide" } else { "Show" }))
                        .clicked()
                    {
                        self.show_otp = !self.show_otp;
                    }
                });
            });

            if let Some(note) = &self.copied_notice {
                ui.colored_label(egui::Color32::from_rgb(130, 225, 160), note);
            }

            ui.add_space(10.0);
            ui.separator();

            // Status line + Start/Stop + Require Accept
            let status_text = if self.host_status.is_empty() {
                if self.host_running { "Listening" } else { "Stopped" }
            } else {
                self.host_status.as_str()
            };
            ui.label(format!("Status: {status_text}"));
            if let Some(err) = &self.host_error {
                ui.colored_label(egui::Color32::from_rgb(255, 110, 110), err);
            }

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let toggle_label = if self.host_running { "Stop Hosting" } else { "Start Hosting" };
                let toggle_text = if self.host_running {
                    egui::RichText::new(toggle_label).color(egui::Color32::from_rgb(255, 140, 130))
                } else {
                    egui::RichText::new(toggle_label).color(egui::Color32::from_rgb(130, 235, 150))
                };
                if ui.add(egui::Button::new(toggle_text).min_size(egui::vec2(120.0, 28.0))).clicked() {
                    *action = Some(ConnectAction::ToggleHost(!self.host_running));
                }
            });

            ui.add_space(6.0);
            if ui
                .checkbox(&mut settings.require_accept, "Require Accept Dialog")
                .on_hover_text("Prompt to accept or reject incoming connections with per-session permissions")
                .changed()
            {
                *action = Some(ConnectAction::SetRequireAccept(settings.require_accept));
            }
        });
    }

    fn show_remote_desk(&mut self, ui: &mut egui::Ui, settings: &mut Settings, action: &mut Option<ConnectAction>) {
        egui::Frame::group(ui.style()).inner_margin(egui::Margin::same(14)).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(egui::RichText::new("Control Remote Desk").size(18.0).strong());
            ui.weak("Enter a 9-digit Device ID or direct host address (ip:port).");
            ui.separator();

            let busy = self.status.is_some();
            let edit = ui.add_enabled(
                !busy,
                egui::TextEdit::singleline(&mut self.address)
                    .hint_text("123 456 789 or 192.168.1.20:21118")
                    .desired_width(ui.available_width()),
            );
            let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let connect = ui.add_enabled(!busy, egui::Button::new("Connect").min_size(egui::vec2(110.0, 28.0)));
                if (connect.clicked() || enter) && !busy {
                    *action = self.try_connect();
                }
                if ui.add(egui::Button::new("Settings").min_size(egui::vec2(96.0, 28.0))).clicked() {
                    *action = Some(ConnectAction::OpenSettings);
                }
            });

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("Mode:");
                ui.selectable_value(&mut settings.mode, crate::settings::Mode::Desktop, "Desktop");
                ui.selectable_value(&mut settings.mode, crate::settings::Mode::Gaming, "Gaming");
            });

            if let Some(s) = &self.status {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(s);
                });
            }
            if let Some(e) = &self.error {
                ui.add_space(4.0);
                ui.colored_label(egui::Color32::from_rgb(255, 110, 110), e);
            }

            if !settings.recent.is_empty() {
                ui.add_space(12.0);
                ui.separator();
                ui.label(egui::RichText::new("Recent Connections").strong());
                let mut remove = None;
                for (i, addr) in settings.recent.iter().enumerate() {
                    ui.horizontal(|ui| {
                        let btn_w = (ui.available_width() - 32.0).max(120.0);
                        if ui.add_enabled(!busy, egui::Button::new(addr).min_size(egui::vec2(btn_w, 22.0))).clicked() {
                            self.address = addr.clone();
                            *action = self.try_connect();
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

            // Tailscale Devices section (1-click zero-VPS P2P)
            ui.add_space(12.0);
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Tailscale Devices").strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Refresh").on_hover_text("Query tailscale status").clicked() {
                        *action = Some(ConnectAction::RefreshTailscale);
                    }
                });
            });

            if !self.tailscale.installed {
                ui.weak("No white IP? Install free Tailscale for 1-click P2P");
                ui.add_space(4.0);
                if ui.button("Download Tailscale").clicked() {
                    *action = Some(ConnectAction::OpenUrl("https://tailscale.com/download".into()));
                }
            } else if !self.tailscale.running {
                let msg = self.tailscale.error.as_deref().unwrap_or("Tailscale is not connected");
                ui.weak(msg);
                ui.add_space(4.0);
                if ui.button("Start / Login Tailscale").clicked() {
                    *action = Some(ConnectAction::StartTailscale);
                }
            } else if self.tailscale.peers.is_empty() {
                ui.weak("Connected to Tailscale — no other devices found yet.");
            } else {
                let mut connect_peer_addr: Option<String> = None;
                egui::ScrollArea::vertical().id_salt("veloxdesk-ts-peers").max_height(180.0).show(ui, |ui| {
                    for peer in &self.tailscale.peers {
                        ui.horizontal(|ui| {
                            let dot_color = if peer.online {
                                egui::Color32::from_rgb(110, 230, 140)
                            } else {
                                egui::Color32::from_rgb(140, 140, 150)
                            };
                            ui.colored_label(dot_color, "●");
                            ui.label(egui::RichText::new(&peer.name).strong());
                            ui.weak(&peer.ip);
                            if !peer.os.is_empty() {
                                ui.weak(format!("({})", peer.os));
                            }
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.add_enabled(!busy, egui::Button::new("Connect").small()).clicked() {
                                    let addr = if peer.ip.contains(':') {
                                        format!("[{}]:{DEFAULT_DIRECT_PORT}", peer.ip)
                                    } else {
                                        format!("{}:{DEFAULT_DIRECT_PORT}", peer.ip)
                                    };
                                    connect_peer_addr = Some(addr);
                                }
                            });
                        });
                    }
                });
                if let Some(addr) = connect_peer_addr {
                    self.address = addr;
                    *action = self.try_connect();
                }
            }
        });
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

    #[test]
    fn connect_screen_renders_headless() {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0))),
            ..Default::default()
        };
        let mut settings = Settings::default();
        settings.recent.push("10.0.0.5:21118".into());
        let mut screen = ConnectScreen::new(&settings);
        screen.device_id = "123456789".into();
        screen.host_running = true;
        screen.host_status = "Listening on :21118".into();
        screen.host_otp = "AB12CD34".into();
        screen.tailscale = TailscaleInfo {
            installed: true,
            running: true,
            self_ip: Some("100.64.1.10".into()),
            self_name: Some("my-pc".into()),
            peers: vec![
                TailscalePeer {
                    name: "gaming-rig".into(),
                    ip: "100.64.1.20".into(),
                    os: "windows".into(),
                    online: true,
                },
                TailscalePeer {
                    name: "linux-box".into(),
                    ip: "100.64.1.30".into(),
                    os: "linux".into(),
                    online: false,
                },
            ],
            error: None,
        };
        let mut out = ctx.run_ui(input, |ui| {
            assert!(screen.show(ui, &mut settings).is_none());
        });
        out.textures_delta.clear();
    }
}
