//! M2 authentication modals: viewer password/OTP prompt and host incoming-session accept dialog.

use veloxdesk_protocol::control::Permissions;

use crate::connect::format_device_id;
use crate::i18n::tr;
use crate::settings::Language;

/// Short human-readable fingerprint (e.g. `abcd1234…ef567890`) for out-of-band verification.
pub fn format_fingerprint(hex: &str) -> String {
    let s = hex.trim();
    if s.len() <= 20 {
        return s.to_string();
    }
    format!("{}…{}", &s[..10], &s[s.len() - 10..])
}

/// Modal state asking the viewer for a password or one-time password (`AuthChallenge`).
#[derive(Debug, Clone)]
pub struct PasswordPrompt {
    pub host_device_id: String,
    pub host_fingerprint: String,
    /// Human-readable description of accepted password methods (e.g. "One-time or permanent password").
    pub methods_hint: String,
    /// Error from a prior attempt, if any.
    pub error: Option<String>,
    pub password: String,
    pub show_password: bool,
}

impl PasswordPrompt {
    pub fn new(host_device_id: String, host_fingerprint: String, methods_hint: String) -> Self {
        Self {
            host_device_id,
            host_fingerprint,
            methods_hint,
            error: None,
            password: String::new(),
            show_password: false,
        }
    }

    /// Draw the modal in English. Returns `Some(PasswordAction)` once the user submits or cancels.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<PasswordAction> {
        self.show_for(ctx, Language::En)
    }

    /// Draw the modal in `lang`. Returns `Some(PasswordAction)` once the user submits or cancels.
    pub fn show_for(&mut self, ctx: &egui::Context, lang: Language) -> Option<PasswordAction> {
        let t = tr(lang);
        let mut action = None;
        egui::Window::new(t.auth_required_title)
            .id(egui::Id::unique("veloxdesk-password-prompt"))
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.set_min_width(340.0);
                if !self.host_device_id.is_empty() {
                    ui.label(t.host_id_line(&format_device_id(&self.host_device_id)));
                }
                if !self.host_fingerprint.is_empty() {
                    ui.weak(t.key_fingerprint_line(&format_fingerprint(&self.host_fingerprint)));
                }
                if !self.methods_hint.is_empty() {
                    ui.add_space(4.0);
                    ui.label(&self.methods_hint);
                }
                ui.add_space(8.0);
                let mut enter = false;
                ui.horizontal(|ui| {
                    let edit = egui::TextEdit::singleline(&mut self.password)
                        .password(!self.show_password)
                        .hint_text(t.enter_password_hint)
                        .desired_width(240.0);
                    let resp = ui.add(edit);
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        enter = true;
                    }
                    if ui.button(if self.show_password { t.hide_btn } else { t.show_btn }).clicked() {
                        self.show_password = !self.show_password;
                    }
                });
                if let Some(err) = &self.error {
                    ui.add_space(4.0);
                    ui.colored_label(egui::Color32::from_rgb(255, 110, 110), err);
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let can_submit = !self.password.is_empty();
                    if ui.add_enabled(can_submit, egui::Button::new(t.connect_btn)).clicked() || (enter && can_submit) {
                        action = Some(PasswordAction::Submit(self.password.clone()));
                    }
                    if ui.button(t.cancel_btn).clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        action = Some(PasswordAction::Cancel);
                    }
                });
            });
        action
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasswordAction {
    Submit(String),
    Cancel,
}

/// Information about a viewer requesting access to this host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingSessionInfo {
    pub peer_device_id: String,
    pub peer_name: String,
    pub peer_fingerprint: String,
    pub peer_public_key: [u8; 32],
}

/// Host user's decision from [`AcceptPrompt`].
#[derive(Debug, Clone, PartialEq)]
pub struct AcceptDecision {
    pub accepted: bool,
    pub permissions: Permissions,
    pub trust_peer: bool,
}

/// Interactive incoming-session approval modal (granular permissions + trust whitelist).
#[derive(Debug, Clone)]
pub struct AcceptPrompt {
    pub peer: IncomingSessionInfo,
    pub permissions: Permissions,
    pub trust_peer: bool,
}

impl AcceptPrompt {
    pub fn new(peer: IncomingSessionInfo, default_permissions: Permissions) -> Self {
        Self { peer, permissions: default_permissions, trust_peer: false }
    }

    /// Draw the approval dialog in English. Returns `Some(AcceptDecision)` once Accept or Reject is clicked.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<AcceptDecision> {
        self.show_for(ctx, Language::En)
    }

    /// Draw the approval dialog in `lang`. Returns `Some(AcceptDecision)` once Accept or Reject is clicked.
    pub fn show_for(&mut self, ctx: &egui::Context, lang: Language) -> Option<AcceptDecision> {
        let t = tr(lang);
        let mut decision = None;
        egui::Window::new(t.incoming_connection_title)
            .id(egui::Id::unique("veloxdesk-accept-prompt"))
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.set_min_width(360.0);
                let caller = if self.peer.peer_name.is_empty() {
                    t.unknown_device
                } else {
                    self.peer.peer_name.as_str()
                };
                ui.label(egui::RichText::new(caller).strong().size(16.0));
                ui.label(t.device_id_line(&format_device_id(&self.peer.peer_device_id)));
                ui.weak(t.ed25519_fingerprint_line(&format_fingerprint(&self.peer.peer_fingerprint)));
                ui.separator();
                ui.label(egui::RichText::new(t.granted_permissions_heading).strong());
                egui::Grid::new("veloxdesk-accept-perms").num_columns(2).spacing([20.0, 4.0]).show(ui, |ui| {
                    ui.checkbox(&mut self.permissions.view, t.perm_view);
                    ui.checkbox(&mut self.permissions.input, t.perm_input);
                    ui.end_row();
                    ui.checkbox(&mut self.permissions.clipboard, t.perm_clipboard);
                    ui.checkbox(&mut self.permissions.file_transfer, t.perm_file_transfer);
                    ui.end_row();
                    ui.checkbox(&mut self.permissions.audio, t.perm_audio);
                    ui.checkbox(&mut self.permissions.tunnel, t.perm_tunnel);
                    ui.end_row();
                    ui.checkbox(&mut self.permissions.terminal, t.perm_terminal);
                    ui.checkbox(&mut self.permissions.privacy_mode, t.perm_privacy_mode);
                    ui.end_row();
                    ui.checkbox(&mut self.permissions.restart, t.perm_restart);
                    ui.end_row();
                });
                ui.separator();
                ui.checkbox(&mut self.trust_peer, t.trust_device_checkbox);
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let accept_btn = egui::Button::new(
                        egui::RichText::new(t.accept_btn).color(egui::Color32::from_rgb(130, 235, 150)),
                    );
                    if ui.add(accept_btn).clicked() {
                        decision = Some(AcceptDecision {
                            accepted: true,
                            permissions: self.permissions.clone(),
                            trust_peer: self.trust_peer,
                        });
                    }
                    let reject_btn = egui::Button::new(
                        egui::RichText::new(t.reject_btn).color(egui::Color32::from_rgb(255, 120, 120)),
                    );
                    if ui.add(reject_btn).clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        decision = Some(AcceptDecision {
                            accepted: false,
                            permissions: Permissions::default(),
                            trust_peer: false,
                        });
                    }
                });
            });
        decision
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_formatting() {
        assert_eq!(format_fingerprint("short"), "short");
        let fp = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert_eq!(format_fingerprint(fp), "0123456789…6789abcdef");
    }

    #[test]
    fn modals_render_headless() {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0))),
            ..Default::default()
        };
        let mut pw = PasswordPrompt::new(
            "123456789".into(),
            "0123456789abcdef0123456789abcdef".into(),
            "Permanent or one-time password".into(),
        );
        let mut accept = AcceptPrompt::new(
            IncomingSessionInfo {
                peer_device_id: "987654321".into(),
                peer_name: "Gaming-PC".into(),
                peer_fingerprint: "abcdef0123456789abcdef0123456789".into(),
                peer_public_key: [7u8; 32],
            },
            Permissions { view: true, input: true, ..Default::default() },
        );
        let mut out = ctx.run_ui(input, |ui| {
            assert!(pw.show(ui.ctx()).is_none());
            assert!(accept.show(ui.ctx()).is_none());
        });
        out.textures_delta.clear();
    }
}
