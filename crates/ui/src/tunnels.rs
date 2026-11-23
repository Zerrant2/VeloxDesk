//! M4 TCP Port Forwarding panel (`local_port -> remote_host:remote_port`).

use crate::i18n::tr;
use crate::settings::Language;

/// One TCP forwarding rule (`127.0.0.1:<local_port>` -> `<remote_host>:<remote_port>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelSpec {
    pub local_port: u16,
    pub remote_host: String,
    pub remote_port: u16,
}

/// Parse `<local_port>:<remote_host>:<remote_port>` (also supports IPv6 `[::1]`) with localized errors.
pub fn parse_tunnel_spec_i18n(s: &str, lang: Language) -> Result<TunnelSpec, String> {
    let t = tr(lang);
    let s = s.trim();
    let (local_str, rest) = s
        .split_once(':')
        .ok_or_else(|| t.err_tunnel_expected_full(s))?;
    let local_port: u16 = local_str
        .parse()
        .map_err(|_| t.err_tunnel_invalid_local_port(local_str))?;
    let (host_str, rport_str) = rest
        .rsplit_once(':')
        .ok_or_else(|| t.err_tunnel_expected_remote(rest))?;
    let remote_host = host_str.trim().trim_matches(['[', ']']).to_string();
    if remote_host.is_empty() {
        return Err(t.err_tunnel_empty_host.into());
    }
    let remote_port: u16 = rport_str
        .parse()
        .map_err(|_| t.err_tunnel_invalid_remote_port(rport_str))?;
    if remote_port == 0 {
        return Err(t.err_tunnel_zero_port.into());
    }
    Ok(TunnelSpec { local_port, remote_host, remote_port })
}

/// Parse `<local_port>:<remote_host>:<remote_port>` (also supports IPv6 `[::1]`).
pub fn parse_tunnel_spec(s: &str) -> Result<TunnelSpec, String> {
    parse_tunnel_spec_i18n(s, Language::En)
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
        self.show_for(ctx, Language::En)
    }

    pub fn show_for(&mut self, ctx: &egui::Context, lang: Language) -> Vec<TunnelAction> {
        let t = tr(lang);
        let mut actions = Vec::new();
        let mut open = self.open;
        egui::Window::new(t.tunnels_title)
            .id(egui::Id::unique("veloxdesk-tunnels"))
            .open(&mut open)
            .default_size(egui::vec2(530.0, 290.0))
            .resizable(true)
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(t.tunnels_local_port_label);
                    ui.add(egui::TextEdit::singleline(&mut self.local_port_text).desired_width(60.0));
                    crate::icons::arrow_right_widget(ui, 14.0);
                    ui.add(
                        egui::TextEdit::singleline(&mut self.remote_host)
                            .hint_text("127.0.0.1")
                            .desired_width(130.0),
                    );
                    ui.label(":");
                    ui.add(egui::TextEdit::singleline(&mut self.remote_port_text).desired_width(60.0));
                    if ui.button(t.tunnels_add_btn).clicked() {
                        let raw = format!(
                            "{}:{}:{}",
                            self.local_port_text.trim(),
                            self.remote_host.trim(),
                            self.remote_port_text.trim()
                        );
                        match parse_tunnel_spec_i18n(&raw, lang) {
                            Ok(spec) => {
                                self.error = None;
                                actions.push(TunnelAction::Add(spec));
                            }
                            Err(e) => self.error = Some(e),
                        }
                    }
                });
                if let Some(err) = &self.error {
                    let pal = crate::theme::palette_for(ui);
                    ui.colored_label(pal.danger, err);
                }
                ui.separator();
                if self.active.is_empty() {
                    ui.weak(t.tunnels_empty);
                } else {
                    egui::Grid::new("veloxdesk-tunnel-list").num_columns(3).striped(true).show(ui, |ui| {
                        ui.strong(t.tunnels_col_local);
                        ui.strong(t.tunnels_col_remote);
                        ui.strong("");
                        ui.end_row();
                        for tun in &self.active {
                            ui.monospace(&tun.local_addr);
                            ui.monospace(format!("{}:{}", tun.remote_host, tun.remote_port));
                            if ui.small_button(t.tunnels_stop_btn).clicked() {
                                actions.push(TunnelAction::Remove(tun.id));
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
