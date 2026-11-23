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
    /// Whether the host accepts a one-time password (OTP).
    pub allow_otp: bool,
    /// Whether the host accepts a permanent password.
    pub allow_perm: bool,
    /// When `true`, typed/pasted ASCII letters are converted to UPPERCASE and whitespace is stripped.
    /// Defaults to `allow_otp && !allow_perm` so OTP-only hosts auto-uppercase while mixed-case
    /// permanent passwords are never broken.
    pub uppercase_mode: bool,
    /// Error from a prior attempt, if any.
    pub error: Option<String>,
    pub password: String,
    pub show_password: bool,
}

impl PasswordPrompt {
    /// Create a password prompt (backwards-compatible default: permanent password mode, `CAPS` off).
    pub fn new(host_device_id: String, host_fingerprint: String, methods_hint: String) -> Self {
        Self::new_with_methods(host_device_id, host_fingerprint, methods_hint, false, true)
    }

    /// Create a password prompt knowing which password methods the host allows.
    ///
    /// - OTP-only (`allow_otp && !allow_perm`): `uppercase_mode` defaults to `true`, `CAPS` pill shown.
    /// - Both (`allow_otp && allow_perm`): `uppercase_mode` defaults to `false`, `CAPS` pill toggleable.
    /// - Permanent-only (`!allow_otp`): `uppercase_mode` is `false`, `CAPS` pill hidden.
    pub fn new_with_methods(
        host_device_id: String,
        host_fingerprint: String,
        methods_hint: String,
        allow_otp: bool,
        allow_perm: bool,
    ) -> Self {
        Self {
            host_device_id,
            host_fingerprint,
            methods_hint,
            allow_otp,
            allow_perm,
            uppercase_mode: allow_otp && !allow_perm,
            error: None,
            password: String::new(),
            show_password: false,
        }
    }

    /// Normalize a password buffer for OTP entry: strip ASCII whitespace and convert ASCII letters
    /// to UPPERCASE without altering non-ASCII characters.
    pub fn normalize_otp_password(s: &mut String) {
        if s.bytes().any(|b| b.is_ascii_lowercase() || b.is_ascii_whitespace()) {
            s.retain(|c| !c.is_ascii_whitespace());
            s.make_ascii_uppercase();
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
                let pal = crate::theme::palette_for(ui);
                ui.set_min_width(360.0);
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
                    if self.uppercase_mode {
                        Self::normalize_otp_password(&mut self.password);
                    }
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        enter = true;
                    }
                    if ui.button(if self.show_password { t.hide_btn } else { t.show_btn }).clicked() {
                        self.show_password = !self.show_password;
                    }
                    if self.allow_otp {
                        let caps_btn = if self.uppercase_mode {
                            egui::Button::new(
                                egui::RichText::new(t.caps_btn).strong().color(pal.badge_active_text),
                            )
                            .fill(pal.badge_active_bg)
                        } else {
                            egui::Button::new(t.caps_btn)
                        };
                        if ui.add(caps_btn).on_hover_text(t.tip_caps).clicked() {
                            self.uppercase_mode = !self.uppercase_mode;
                            if self.uppercase_mode {
                                Self::normalize_otp_password(&mut self.password);
                            }
                        }
                    }
                });
                if let Some(err) = &self.error {
                    ui.add_space(4.0);
                    ui.colored_label(pal.danger, err);
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
                    ui.checkbox(&mut self.permissions.allow_chat, t.perm_chat);
                    ui.checkbox(&mut self.permissions.allow_power, t.perm_power);
                    ui.end_row();
                    ui.checkbox(&mut self.permissions.restart, t.perm_restart);
                    ui.end_row();
                });
                ui.separator();
                ui.checkbox(&mut self.trust_peer, t.trust_device_checkbox);
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let pal = crate::theme::palette_for(ui);
                    let accept_btn = egui::Button::new(
                        egui::RichText::new(t.accept_btn).color(pal.success),
                    );
                    if ui.add(accept_btn).clicked() {
                        decision = Some(AcceptDecision {
                            accepted: true,
                            permissions: self.permissions.clone(),
                            trust_peer: self.trust_peer,
                        });
                    }
                    let reject_btn = egui::Button::new(
                        egui::RichText::new(t.reject_btn).color(pal.danger),
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

    fn find_text_rect(shape: &egui::epaint::Shape, needle: &str) -> Option<egui::Rect> {
        match shape {
            egui::epaint::Shape::Vec(v) => v.iter().find_map(|s| find_text_rect(s, needle)),
            egui::epaint::Shape::Text(ts) if ts.galley.text().contains(needle) => {
                Some(ts.galley.rect.translate(ts.pos.to_vec2()))
            }
            _ => None,
        }
    }

    #[test]
    fn otp_uppercase_defaults_normalization_and_caps_toggle() {
        // 1. OTP-only: uppercase_mode defaults to ON; lowercases and spaces normalize in real time.
        let mut otp_only = PasswordPrompt::new_with_methods(
            "123456789".into(),
            "abcd".into(),
            "Enter one-time password".into(),
            true,
            false,
        );
        assert!(otp_only.allow_otp);
        assert!(!otp_only.allow_perm);
        assert!(otp_only.uppercase_mode);
        otp_only.password = "ab12 cd34\t".into();

        let ctx = egui::Context::default();
        let make_input = |events: Vec<egui::Event>| egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0))),
            events,
            ..Default::default()
        };
        let mut out = ctx.run_ui(make_input(Vec::new()), |ui| {
            let _ = otp_only.show_for(ui.ctx(), Language::En);
        });
        out.textures_delta.clear();
        // Second frame completes anchored Window layout pass so shapes are emitted.
        out = ctx.run_ui(make_input(Vec::new()), |ui| {
            let _ = otp_only.show_for(ui.ctx(), Language::En);
        });
        out.textures_delta.clear();
        assert_eq!(otp_only.password, "AB12CD34");
        // CAPS pill must be visible when allow_otp is true.
        assert!(
            out.shapes.iter().any(|c| find_text_rect(&c.shape, "CAPS").is_some()),
            "CAPS toggle pill must render for OTP-only host"
        );

        // 2. Permanent-only: uppercase_mode is OFF, mixed case preserved, CAPS pill hidden.
        let mut perm_only = PasswordPrompt::new_with_methods(
            "123456789".into(),
            "abcd".into(),
            "Enter permanent password".into(),
            false,
            true,
        );
        assert!(!perm_only.allow_otp);
        assert!(perm_only.allow_perm);
        assert!(!perm_only.uppercase_mode);
        perm_only.password = "MySecret Pass123".into();
        let ctx_perm = egui::Context::default();
        let mut out_perm = ctx_perm.run_ui(make_input(Vec::new()), |ui| {
            let _ = perm_only.show_for(ui.ctx(), Language::En);
        });
        out_perm.textures_delta.clear();
        out_perm = ctx_perm.run_ui(make_input(Vec::new()), |ui| {
            let _ = perm_only.show_for(ui.ctx(), Language::En);
        });
        out_perm.textures_delta.clear();
        assert_eq!(perm_only.password, "MySecret Pass123");
        assert!(
            !out_perm.shapes.iter().any(|c| find_text_rect(&c.shape, "CAPS").is_some()),
            "CAPS toggle pill must be hidden for permanent-only host"
        );

        // 3. Both mode: uppercase_mode defaults OFF (preserves mixed-case permanent password),
        //    CAPS pill is visible and clicking it turns uppercase_mode ON & normalizes buffer.
        let mut both = PasswordPrompt::new_with_methods(
            "123456789".into(),
            "abcd".into(),
            "Enter permanent or one-time password".into(),
            true,
            true,
        );
        assert!(both.allow_otp && both.allow_perm && !both.uppercase_mode);
        both.password = "ab 99".into();
        let ctx_both = egui::Context::default();
        for _ in 0..2 {
            let mut o = ctx_both.run_ui(make_input(Vec::new()), |ui| {
                let _ = both.show_for(ui.ctx(), Language::En);
            });
            o.textures_delta.clear();
            if let Some(rect) = o.shapes.iter().find_map(|c| find_text_rect(&c.shape, "CAPS")) {
                let pos = rect.center();
                assert_eq!(both.password, "ab 99");
                for pressed in [true, false] {
                    let mut c = ctx_both.run_ui(
                        make_input(vec![
                            egui::Event::PointerMoved(pos),
                            egui::Event::PointerButton {
                                pos,
                                button: egui::PointerButton::Primary,
                                pressed,
                                modifiers: egui::Modifiers::NONE,
                            },
                        ]),
                        |ui| {
                            let _ = both.show_for(ui.ctx(), Language::En);
                        },
                    );
                    c.textures_delta.clear();
                }
                break;
            }
        }
        assert!(both.uppercase_mode, "clicking CAPS must enable uppercase_mode");
        assert_eq!(both.password, "AB99");
    }
}
