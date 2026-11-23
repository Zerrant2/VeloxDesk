//! Remote host power confirmation modal and auto-reconnect status overlay (v0.5.0).

use veloxdesk_protocol::control::SystemPowerAction;

use crate::i18n::tr;
use crate::settings::Language;

/// Localized display name for a [`SystemPowerAction`].
pub fn power_action_label(action: SystemPowerAction, lang: Language) -> &'static str {
    let t = tr(lang);
    match action {
        SystemPowerAction::Reboot => t.power_reboot,
        SystemPowerAction::RebootSafeMode => t.power_reboot_safe_mode,
        SystemPowerAction::LockScreen => t.power_lock_screen,
        SystemPowerAction::Sleep => t.power_sleep,
    }
}

/// Localized warning description shown inside [`PowerConfirmDialog`].
pub fn power_action_warning(action: SystemPowerAction, lang: Language) -> &'static str {
    let t = tr(lang);
    match action {
        SystemPowerAction::Reboot => t.power_warn_reboot,
        SystemPowerAction::RebootSafeMode => t.power_warn_reboot_safe_mode,
        SystemPowerAction::LockScreen => t.power_warn_lock_screen,
        SystemPowerAction::Sleep => t.power_warn_sleep,
    }
}

/// Outcome returned by [`PowerConfirmDialog::show_for`] once the user confirms or cancels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerConfirmOutcome {
    Confirmed(SystemPowerAction),
    Cancelled,
}

/// Modal confirmation dialog gating destructive remote power actions (`Reboot`, `Safe Mode`, `Lock`, `Sleep`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PowerConfirmDialog {
    pub action: SystemPowerAction,
    pub host_name: String,
}

impl PowerConfirmDialog {
    pub fn new(action: SystemPowerAction, host_name: String) -> Self {
        Self { action, host_name }
    }

    pub fn show(&self, ctx: &egui::Context) -> Option<PowerConfirmOutcome> {
        self.show_for(ctx, Language::En)
    }

    pub fn show_for(&self, ctx: &egui::Context, lang: Language) -> Option<PowerConfirmOutcome> {
        let t = tr(lang);
        let mut outcome = None;
        egui::Window::new(t.power_confirm_title)
            .id(egui::Id::unique("veloxdesk-power-confirm"))
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                let pal = crate::theme::palette_for(ui);
                ui.set_min_width(360.0);
                ui.set_max_width(420.0);
                let action_label = power_action_label(self.action, lang);
                ui.label(
                    egui::RichText::new(t.fmt_power_confirm_target(&self.host_name, action_label))
                        .strong()
                        .size(15.0),
                );
                ui.add_space(6.0);
                ui.add(egui::Label::new(
                    egui::RichText::new(power_action_warning(self.action, lang)).color(pal.warning),
                ).wrap());
                if matches!(self.action, SystemPowerAction::Reboot | SystemPowerAction::RebootSafeMode) {
                    ui.add_space(6.0);
                    ui.add(egui::Label::new(egui::RichText::new(t.power_reboot_hint_reconnect).weak()).wrap());
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    let confirm_btn = egui::Button::new(
                        egui::RichText::new(t.power_confirm_btn).strong().color(pal.danger),
                    )
                    .min_size(egui::vec2(100.0, 26.0));
                    if ui.add(confirm_btn).clicked() {
                        outcome = Some(PowerConfirmOutcome::Confirmed(self.action));
                    }
                    if ui.add(egui::Button::new(t.cancel_btn).min_size(egui::vec2(88.0, 26.0))).clicked()
                        || ui.input(|i| i.key_pressed(egui::Key::Escape))
                    {
                        outcome = Some(PowerConfirmOutcome::Cancelled);
                    }
                });
            });
        outcome
    }
}

/// Live auto-reconnect state rendered by [`ReconnectOverlay`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconnectBannerState {
    pub active: bool,
    pub target: String,
    pub attempt: u32,
    pub elapsed_secs: u64,
    pub max_secs: u64,
    pub next_retry_secs: u64,
    pub last_error: Option<String>,
}

/// Action returned by [`ReconnectOverlay`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconnectAction {
    Cancel,
}

/// Centered overlay displayed while VeloxDesk automatically reconnects after a remote reboot.
#[derive(Debug, Default)]
pub struct ReconnectOverlay;

impl ReconnectOverlay {
    pub fn show(ctx: &egui::Context, state: &ReconnectBannerState) -> Option<ReconnectAction> {
        Self::show_for(ctx, state, Language::En)
    }

    pub fn show_for(ctx: &egui::Context, state: &ReconnectBannerState, lang: Language) -> Option<ReconnectAction> {
        if !state.active {
            return None;
        }
        let t = tr(lang);
        let mut action = None;
        egui::Area::new(egui::Id::unique("veloxdesk-reconnect-overlay"))
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let pal = crate::theme::palette_for(ui);
                crate::theme::overlay_frame(ui)
                    .inner_margin(egui::Margin::same(16))
                    .show(ui, |ui| {
                        ui.set_min_width(340.0);
                        ui.set_max_width(420.0);
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(egui::RichText::new(t.reconnect_title).strong().size(15.0));
                        });
                        if !state.target.is_empty() {
                            ui.add_space(4.0);
                            ui.label(t.fmt_reconnect_target(&state.target));
                        }
                        ui.add_space(4.0);
                        let max_secs = state.max_secs.max(1);
                        ui.weak(t.fmt_reconnect_progress(
                            state.attempt.max(1),
                            state.next_retry_secs,
                            state.elapsed_secs.min(max_secs),
                            max_secs,
                        ));
                        ui.add_space(6.0);
                        let frac = (state.elapsed_secs as f32 / max_secs as f32).clamp(0.0, 1.0);
                        ui.add(egui::ProgressBar::new(frac));
                        if let Some(err) = &state.last_error
                            && !err.is_empty()
                        {
                            ui.add_space(4.0);
                            ui.add(egui::Label::new(egui::RichText::new(err).small().color(pal.warning)).wrap());
                        }
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            if ui
                                .add(egui::Button::new(egui::RichText::new(t.cancel_btn).color(pal.danger)))
                                .clicked()
                                || ui.input(|i| i.key_pressed(egui::Key::Escape))
                            {
                                action = Some(ReconnectAction::Cancel);
                            }
                        });
                    });
            });
        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_confirm_and_reconnect_overlay_render_headless() {
        let ctx = egui::Context::default();
        let make_input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0))),
            ..Default::default()
        };
        let dlg = PowerConfirmDialog::new(SystemPowerAction::RebootSafeMode, "Gaming-Rig".into());
        let banner = ReconnectBannerState {
            active: true,
            target: "192.168.1.20:21118".into(),
            attempt: 3,
            elapsed_secs: 24,
            max_secs: 300,
            next_retry_secs: 4,
            last_error: Some("connection timed out".into()),
        };

        for lang in Language::ALL {
            let mut out = ctx.run_ui(make_input(), |ui| {
                assert!(dlg.show_for(ui.ctx(), lang).is_none());
                assert!(ReconnectOverlay::show_for(ui.ctx(), &banner, lang).is_none());
            });
            out.textures_delta.clear();
        }

        // Inactive banner renders nothing and returns None.
        let inactive = ReconnectBannerState::default();
        let mut out = ctx.run_ui(make_input(), |ui| {
            assert!(ReconnectOverlay::show(ui.ctx(), &inactive).is_none());
        });
        out.textures_delta.clear();
    }
}
