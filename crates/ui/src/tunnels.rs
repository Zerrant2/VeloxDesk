//! M4 TCP Port Forwarding panel (`local_port -> remote_host:remote_port`).

/// One TCP forwarding rule (`127.0.0.1:<local_port>` → `<remote_host>:<remote_port>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelSpec {
    pub local_port: u16,
    pub remote_host: String,
    pub remote_port: u16,
}

/// Parse `<local_port>:<remote_host>:<remote_port>` (also supports IPv6 `[::1]`).
pub fn parse_tunnel_spec(s: &str) -> Result<TunnelSpec, String> {
    let s = s.trim();
    let (local_str, rest) = s
        .split_once(':')
        .ok_or_else(|| format!("expected local_port:remote_host:remote_port, got {s:?}"))?;
    let local_port: u16 = local_str
        .parse()
        .map_err(|_| format!("invalid local port {local_str:?}"))?;
    let (host_str, rport_str) = rest
        .rsplit_once(':')
        .ok_or_else(|| format!("expected remote_host:remote_port, got {rest:?}"))?;
    let remote_host = host_str.trim().trim_matches(['[', ']']).to_string();
    if remote_host.is_empty() {
        return Err("remote host must not be empty".into());
    }
    let remote_port: u16 = rport_str
        .parse()
        .map_err(|_| format!("invalid remote port {rport_str:?}"))?;
    if remote_port == 0 {
        return Err("remote port must be > 0".into());
    }
    Ok(TunnelSpec { local_port, remote_host, remote_port })
}

/// An active tunnel displayed in [`TunnelsWindow`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveTunnel {
    pub id: u64,
    pub local_addr: String,
    pub remote_host: String,
    pub remote_port: u16,
}

/// Actions emitted by [`TunnelsWindow`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelAction {
    Add(TunnelSpec),
    Remove(u64),
}

/// TCP Port Forwarding egui window.
#[derive(Debug, Clone)]
pub struct TunnelsWindow {
    pub open: bool,
    pub local_port_text: String,
    pub remote_host: String,
    pub remote_port_text: String,
    pub active: Vec<ActiveTunnel>,
    pub error: Option<String>,
}

impl Default for TunnelsWindow {
    fn default() -> Self {
        Self {
            open: false,
            local_port_text: "8080".into(),
            remote_host: "127.0.0.1".into(),
            remote_port_text: "80".into(),
            active: Vec::new(),
            error: None,
        }
    }
}

impl TunnelsWindow {
    pub fn show(&mut self, ctx: &egui::Context) -> Vec<TunnelAction> {
        let mut actions = Vec::new();
        let mut open = self.open;
        egui::Window::new("TCP Port Forwarding")
            .id(egui::Id::unique("veloxdesk-tunnels"))
            .open(&mut open)
            .default_size(egui::vec2(480.0, 280.0))
            .resizable(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Local port:");
                    ui.add(egui::TextEdit::singleline(&mut self.local_port_text).desired_width(60.0));
                    ui.label("→");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.remote_host)
                            .hint_text("127.0.0.1")
                            .desired_width(130.0),
                    );
                    ui.label(":");
                    ui.add(egui::TextEdit::singleline(&mut self.remote_port_text).desired_width(60.0));
                    if ui.button("Add").clicked() {
                        let raw = format!(
                            "{}:{}:{}",
                            self.local_port_text.trim(),
                            self.remote_host.trim(),
                            self.remote_port_text.trim()
                        );
                        match parse_tunnel_spec(&raw) {
                            Ok(spec) => {
                                self.error = None;
                                actions.push(TunnelAction::Add(spec));
                            }
                            Err(e) => self.error = Some(e),
                        }
                    }
                });
                if let Some(err) = &self.error {
                    ui.colored_label(egui::Color32::from_rgb(255, 110, 110), err);
                }
                ui.separator();
                if self.active.is_empty() {
                    ui.weak("No active port forwards.");
                } else {
                    egui::Grid::new("veloxdesk-tunnel-list").num_columns(3).striped(true).show(ui, |ui| {
                        ui.strong("Local");
                        ui.strong("Remote");
                        ui.strong("");
                        ui.end_row();
                        for t in &self.active {
                            ui.monospace(&t.local_addr);
                            ui.monospace(format!("{}:{}", t.remote_host, t.remote_port));
                            if ui.small_button("Stop").clicked() {
                                actions.push(TunnelAction::Remove(t.id));
                            }
                            ui.end_row();
                        }
                    });
                }
            });
        self.open = open;
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_specs() {
        let s = parse_tunnel_spec("8080:127.0.0.1:80").unwrap();
        assert_eq!((s.local_port, s.remote_host.as_str(), s.remote_port), (8080, "127.0.0.1", 80));
        let v6 = parse_tunnel_spec("0:[::1]:5432").unwrap();
        assert_eq!((v6.local_port, v6.remote_host.as_str(), v6.remote_port), (0, "::1", 5432));
        assert!(parse_tunnel_spec("bad").is_err());
        assert!(parse_tunnel_spec("8080:127.0.0.1:0").is_err());
    }

    #[test]
    fn tunnels_renders_headless() {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0))),
            ..Default::default()
        };
        let mut tw = TunnelsWindow {
            open: true,
            active: vec![ActiveTunnel {
                id: 1,
                local_addr: "127.0.0.1:8080".into(),
                remote_host: "127.0.0.1".into(),
                remote_port: 80,
            }],
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| {
            assert!(tw.show(ui.ctx()).is_empty());
        });
        out.textures_delta.clear();
    }
}
