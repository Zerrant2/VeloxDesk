//! Session toolbar (top-center overlay) and settings window.

use veloxdesk_protocol::control::DisplayInfo;

use crate::settings::{Mode, Present, Quality, Scale, Settings};

/// What the toolbar asks the session to do.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolbarAction {
    SetMode(Mode),
    SwitchDisplay(u32),
    SetQuality(Quality),
    SetPresent(Present),
    SetScale(Scale),
    ToggleHud,
    ToggleFullscreen,
    OpenSettings,
    Disconnect,
}

/// Session information shown in the toolbar.
#[derive(Debug, Clone, Default)]
pub struct SessionInfo {
    pub host_name: String,
    pub displays: Vec<DisplayInfo>,
    pub active_display: u32,
    pub hud_visible: bool,
}

#[derive(Debug, Default)]
pub struct SessionToolbar {
    pub expanded: bool,
}

impl SessionToolbar {
    /// Draw the toolbar. Returns actions; `settings` is updated in place for choices.
    pub fn show(&mut self, ui: &mut egui::Ui, info: &SessionInfo, settings: &mut Settings) -> Vec<ToolbarAction> {
        let mut out = Vec::new();
        egui::Area::new(egui::Id::unique("veloxdesk-toolbar"))
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 0.0))
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                egui::Frame::new()
                    .fill(egui::Color32::from_black_alpha(200))
                    .corner_radius(egui::CornerRadius { nw: 0, ne: 0, sw: 6, se: 6 })
                    .inner_margin(egui::Margin::symmetric(8, 3))
                    .show(ui, |ui| {
                        if !self.expanded {
                            if ui.small_button("▾ VeloxDesk").on_hover_text("Session toolbar").clicked() {
                                self.expanded = true;
                            }
                            return;
                        }
                        ui.horizontal(|ui| {
                            if ui.small_button("▴").clicked() {
                                self.expanded = false;
                            }
                            if !info.host_name.is_empty() {
                                ui.label(egui::RichText::new(&info.host_name).strong());
                                ui.separator();
                            }
                            for m in [Mode::Desktop, Mode::Gaming] {
                                if ui.selectable_label(settings.mode == m, m.label()).clicked() && settings.mode != m {
                                    settings.mode = m;
                                    out.push(ToolbarAction::SetMode(m));
                                }
                            }
                            ui.separator();
                            if info.displays.len() > 1 {
                                let current = info
                                    .displays
                                    .iter()
                                    .find(|d| d.index == info.active_display)
                                    .map(display_label)
                                    .unwrap_or_else(|| format!("Display {}", info.active_display));
                                egui::ComboBox::from_id_salt("veloxdesk-display")
                                    .selected_text(current)
                                    .show_ui(ui, |ui| {
                                        for d in &info.displays {
                                            if ui.selectable_label(d.index == info.active_display, display_label(d)).clicked()
                                                && d.index != info.active_display
                                            {
                                                out.push(ToolbarAction::SwitchDisplay(d.index));
                                            }
                                        }
                                    });
                            }
                            egui::ComboBox::from_id_salt("veloxdesk-quality")
                                .selected_text(settings.quality.label())
                                .show_ui(ui, |ui| {
                                    for q in Quality::ALL {
                                        if ui.selectable_label(settings.quality == q, q.label()).clicked()
                                            && settings.quality != q
                                        {
                                            settings.quality = q;
                                            out.push(ToolbarAction::SetQuality(q));
                                        }
                                    }
                                });
                            egui::ComboBox::from_id_salt("veloxdesk-scale")
                                .selected_text(settings.scale.label())
                                .show_ui(ui, |ui| {
                                    for s in Scale::ALL {
                                        if ui.selectable_label(settings.scale == s, s.label()).clicked() && settings.scale != s {
                                            settings.scale = s;
                                            out.push(ToolbarAction::SetScale(s));
                                        }
                                    }
                                });
                            if ui.selectable_label(info.hud_visible, "HUD").on_hover_text("Ctrl+Shift+H").clicked() {
                                out.push(ToolbarAction::ToggleHud);
                            }
                            if ui.small_button("Fullscreen").on_hover_text("F11").clicked() {
                                out.push(ToolbarAction::ToggleFullscreen);
                            }
                            if ui.small_button("⚙").on_hover_text("Settings").clicked() {
                                out.push(ToolbarAction::OpenSettings);
                            }
                            if ui
                                .add(egui::Button::new(egui::RichText::new("Disconnect").color(egui::Color32::from_rgb(255, 120, 120))))
                                .clicked()
                            {
                                out.push(ToolbarAction::Disconnect);
                            }
                        });
                    });
            });
        out
    }
}

fn display_label(d: &DisplayInfo) -> String {
    let name = if d.name.is_empty() { format!("Display {}", d.index + 1) } else { d.name.clone() };
    format!("{name} ({}x{}{})", d.width, d.height, if d.primary { ", primary" } else { "" })
}

/// Settings window (skeleton). Returns actions for settings that apply immediately.
pub fn settings_window(ctx: &egui::Context, open: &mut bool, settings: &mut Settings) -> Vec<ToolbarAction> {
    let mut out = Vec::new();
    let before = settings.clone();
    egui::Window::new("Settings").open(open).resizable(false).collapsible(false).show(ctx, |ui| {
        egui::Grid::new("veloxdesk-settings").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
            ui.label("Default mode");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut settings.mode, Mode::Desktop, "Desktop");
                ui.selectable_value(&mut settings.mode, Mode::Gaming, "Gaming");
            });
            ui.end_row();

            ui.label("Quality");
            egui::ComboBox::from_id_salt("set-quality").selected_text(settings.quality.label()).show_ui(ui, |ui| {
                for q in Quality::ALL {
                    ui.selectable_value(&mut settings.quality, q, q.label());
                }
            });
            ui.end_row();

            ui.label("Presentation");
            egui::ComboBox::from_id_salt("set-present").selected_text(settings.present.label()).show_ui(ui, |ui| {
                for p in Present::ALL {
                    ui.selectable_value(&mut settings.present, p, p.label());
                }
            });
            ui.end_row();

            ui.label("Scaling");
            egui::ComboBox::from_id_salt("set-scale").selected_text(settings.scale.label()).show_ui(ui, |ui| {
                for s in Scale::ALL {
                    ui.selectable_value(&mut settings.scale, s, s.label());
                }
            });
            ui.end_row();

            ui.label("Performance HUD");
            ui.checkbox(&mut settings.show_hud, "Show on connect (Ctrl+Shift+H)");
            ui.end_row();

            ui.label("Decoder");
            ui.checkbox(&mut settings.prefer_hw_decode, "Prefer hardware decoding");
            ui.end_row();

            ui.label("Gaming mouse");
            ui.checkbox(&mut settings.relative_mouse_in_gaming, "Lock pointer, relative motion");
            ui.end_row();

            ui.label("Rendezvous server");
            ui.add(
                egui::TextEdit::singleline(&mut settings.rendezvous_server)
                    .hint_text("127.0.0.1:21116")
                    .desired_width(200.0),
            );
            ui.end_row();
        });
        ui.add_space(6.0);
        ui.weak("Audio, clipboard and file transfer settings arrive in later milestones.");
    });
    if *settings != before {
        if settings.present != before.present {
            out.push(ToolbarAction::SetPresent(settings.present));
        }
        if settings.scale != before.scale {
            out.push(ToolbarAction::SetScale(settings.scale));
        }
        if settings.quality != before.quality {
            out.push(ToolbarAction::SetQuality(settings.quality));
        }
        if settings.mode != before.mode {
            out.push(ToolbarAction::SetMode(settings.mode));
        }
        settings.save_default();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(f: impl FnMut(&mut egui::Ui)) {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0))),
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, f);
        out.textures_delta.clear();
    }

    #[test]
    fn widgets_render_headless() {
        let mut settings = Settings::default();
        let mut tb = SessionToolbar { expanded: true };
        let info = SessionInfo {
            host_name: "host".into(),
            displays: vec![
                DisplayInfo { index: 0, name: "A".into(), width: 1920, height: 1080, primary: true, ..Default::default() },
                DisplayInfo { index: 1, name: "B".into(), width: 2560, height: 1440, ..Default::default() },
            ],
            active_display: 0,
            hud_visible: false,
        };
        let mut screen = crate::connect::ConnectScreen::new(&settings);
        let mut open = true;
        run(|ui| {
            let a = tb.show(ui, &info, &mut settings);
            assert!(a.is_empty());
            let _ = screen.show(ui, &mut settings);
            let a = settings_window(ui.ctx(), &mut open, &mut Settings::default());
            assert!(a.is_empty());
        });
        assert_eq!(display_label(&info.displays[0]), "A (1920x1080, primary)");
    }
}
