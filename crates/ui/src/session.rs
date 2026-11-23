//! Session toolbar (top-center overlay) and settings window.

use std::path::Path;

use veloxdesk_protocol::control::DisplayInfo;

use crate::connect::UpdateUiState;
use crate::i18n::tr;
use crate::settings::{FpsCap, Language, Mode, Present, Quality, Scale, Settings};

/// What the toolbar asks the session to do.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolbarAction {
    SetLanguage(Language),
    SetMode(Mode),
    SwitchDisplay(u32),
    SetQuality(Quality),
    SetFpsCap(FpsCap),
    SetPresent(Present),
    SetScale(Scale),
    SetAudio(bool),
    SetMic(bool),
    SetClipboardSync(bool),
    SetPrivacyMode(bool),
    SetHud(bool),
    ToggleFiles,
    ToggleTerminal,
    ToggleTunnels,
    TogglePointerLock,
    ToggleHud,
    ToggleFullscreen,
    OpenSettings,
    CheckForUpdates,
    StartUpdate,
    Disconnect,
}

/// Session information shown in the toolbar.
#[derive(Debug, Clone, Default)]
pub struct SessionInfo {
    pub host_name: String,
    pub displays: Vec<DisplayInfo>,
    pub active_display: u32,
    pub hud_visible: bool,
    pub pointer_locked: bool,
    pub active_gamepads: u8,
    pub privacy_mode: bool,
    pub files_open: bool,
    pub terminal_open: bool,
    pub tunnels_open: bool,
}

#[derive(Debug, Default)]
pub struct SessionToolbar {
    pub expanded: bool,
}

impl SessionToolbar {
    /// Draw the toolbar. Returns actions; `settings` is updated in place for choices.
    pub fn show(&mut self, ui: &mut egui::Ui, info: &SessionInfo, settings: &mut Settings) -> Vec<ToolbarAction> {
        let lang = settings.language;
        let t = tr(lang);
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
                            ui.horizontal(|ui| {
                                if ui
                                    .small_button((crate::icons::chevron_down_atom(10.0), t.app_title))
                                    .on_hover_text(t.session_toolbar_tooltip)
                                    .clicked()
                                {
                                    self.expanded = true;
                                }
                                if info.pointer_locked {
                                    ui.colored_label(
                                        egui::Color32::from_rgb(130, 235, 150),
                                        t.pointer_locked_pill,
                                    );
                                }
                            });
                            return;
                        }
                        ui.set_max_width((ui.ctx().content_rect().width() - 32.0).max(320.0));
                        ui.horizontal_wrapped(|ui| {
                            if crate::icons::icon_small_button(
                                ui,
                                egui::vec2(20.0, 18.0),
                                crate::icons::chevron_up_atom(10.0),
                            )
                            .on_hover_text(t.collapse_toolbar_tooltip)
                            .clicked()
                            {
                                self.expanded = false;
                            }
                            if !info.host_name.is_empty() {
                                ui.label(egui::RichText::new(&info.host_name).strong());
                                ui.separator();
                            }
                            for m in [Mode::Desktop, Mode::Gaming] {
                                if ui.selectable_label(settings.mode == m, m.label_for(lang)).clicked()
                                    && settings.mode != m
                                {
                                    settings.mode = m;
                                    out.push(ToolbarAction::SetMode(m));
                                }
                            }
                            let prof = settings.active_mut();
                            ui.separator();
                            if info.displays.len() > 1 {
                                let current = info
                                    .displays
                                    .iter()
                                    .find(|d| d.index == info.active_display)
                                    .map(|d| t.display_label(d))
                                    .unwrap_or_else(|| t.display_numbered(info.active_display + 1));
                                egui::ComboBox::from_id_salt("veloxdesk-display")
                                    .selected_text(current)
                                    .show_ui(ui, |ui| {
                                        for d in &info.displays {
                                            if ui
                                                .selectable_label(d.index == info.active_display, t.display_label(d))
                                                .clicked()
                                                && d.index != info.active_display
                                            {
                                                out.push(ToolbarAction::SwitchDisplay(d.index));
                                            }
                                        }
                                    });
                            }
                            egui::ComboBox::from_id_salt("veloxdesk-quality")
                                .selected_text(prof.quality.label_for(lang))
                                .show_ui(ui, |ui| {
                                    for q in Quality::ALL {
                                        if ui.selectable_label(prof.quality == q, q.label_for(lang)).clicked()
                                            && prof.quality != q
                                        {
                                            prof.quality = q;
                                            out.push(ToolbarAction::SetQuality(q));
                                        }
                                    }
                                });
                            egui::ComboBox::from_id_salt("veloxdesk-scale")
                                .selected_text(prof.scale.label_for(lang))
                                .show_ui(ui, |ui| {
                                    for s in Scale::ALL {
                                        if ui.selectable_label(prof.scale == s, s.label_for(lang)).clicked()
                                            && prof.scale != s
                                        {
                                            prof.scale = s;
                                            out.push(ToolbarAction::SetScale(s));
                                        }
                                    }
                                });
                            egui::ComboBox::from_id_salt("veloxdesk-fps")
                                .selected_text(prof.fps_cap.label_for(lang))
                                .show_ui(ui, |ui| {
                                    for f in FpsCap::ALL {
                                        if ui.selectable_label(prof.fps_cap == f, f.label_for(lang)).clicked()
                                            && prof.fps_cap != f
                                        {
                                            prof.fps_cap = f;
                                            out.push(ToolbarAction::SetFpsCap(f));
                                        }
                                    }
                                });
                            if ui
                                .selectable_label(prof.audio_enabled, t.toolbar_audio)
                                .on_hover_text(t.toolbar_audio_tooltip)
                                .clicked()
                            {
                                prof.audio_enabled = !prof.audio_enabled;
                                out.push(ToolbarAction::SetAudio(prof.audio_enabled));
                            }
                            if ui
                                .selectable_label(prof.mic_enabled, t.toolbar_mic)
                                .on_hover_text(t.toolbar_mic_tooltip)
                                .clicked()
                            {
                                prof.mic_enabled = !prof.mic_enabled;
                                out.push(ToolbarAction::SetMic(prof.mic_enabled));
                            }
                            if ui
                                .selectable_label(
                                    info.pointer_locked,
                                    if info.pointer_locked { t.toolbar_locked } else { t.toolbar_lock },
                                )
                                .on_hover_text(t.toolbar_lock_tooltip)
                                .clicked()
                            {
                                out.push(ToolbarAction::TogglePointerLock);
                            }
                            if info.active_gamepads > 0 {
                                ui.label(t.toolbar_pads(info.active_gamepads));
                            }
                            if ui
                                .selectable_label(info.files_open, t.toolbar_files)
                                .on_hover_text(t.files_title)
                                .clicked()
                            {
                                out.push(ToolbarAction::ToggleFiles);
                            }
                            if ui
                                .selectable_label(info.terminal_open, t.toolbar_term)
                                .on_hover_text(t.terminal_title)
                                .clicked()
                            {
                                out.push(ToolbarAction::ToggleTerminal);
                            }
                            if ui
                                .selectable_label(info.tunnels_open, t.toolbar_tunnels)
                                .on_hover_text(t.tunnels_title)
                                .clicked()
                            {
                                out.push(ToolbarAction::ToggleTunnels);
                            }
                            if ui
                                .selectable_label(info.privacy_mode, t.toolbar_privacy)
                                .on_hover_text(t.toolbar_privacy_tooltip)
                                .clicked()
                            {
                                out.push(ToolbarAction::SetPrivacyMode(!info.privacy_mode));
                            }
                            if ui
                                .selectable_label(prof.clipboard_sync, t.toolbar_clip)
                                .on_hover_text(t.toolbar_clip_tooltip)
                                .clicked()
                            {
                                prof.clipboard_sync = !prof.clipboard_sync;
                                out.push(ToolbarAction::SetClipboardSync(prof.clipboard_sync));
                            }
                            if ui
                                .selectable_label(info.hud_visible, t.toolbar_hud)
                                .on_hover_text("Ctrl+Shift+H")
                                .clicked()
                            {
                                out.push(ToolbarAction::ToggleHud);
                            }
                            if ui.small_button(t.toolbar_fullscreen).on_hover_text("Ctrl+Shift+F").clicked() {
                                out.push(ToolbarAction::ToggleFullscreen);
                            }
                            if crate::icons::icon_small_button(
                                ui,
                                egui::vec2(22.0, 18.0),
                                crate::icons::gear_atom(12.0),
                            )
                            .on_hover_text(t.settings_title)
                            .clicked()
                            {
                                out.push(ToolbarAction::OpenSettings);
                            }
                            if ui
                                .add(egui::Button::new(
                                    egui::RichText::new(t.toolbar_disconnect)
                                        .color(egui::Color32::from_rgb(255, 120, 120)),
                                ))
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

#[cfg(test)]
fn display_label(d: &DisplayInfo) -> String {
    tr(Language::En).display_label(d)
}

/// Settings window (uses default settings path). Returns actions for settings that apply immediately.
pub fn settings_window(ctx: &egui::Context, open: &mut bool, settings: &mut Settings) -> Vec<ToolbarAction> {
    settings_window_at(ctx, open, settings, None)
}

/// Settings window with optional `--portable` override path. Returns actions for settings that apply immediately.
pub fn settings_window_at(
    ctx: &egui::Context,
    open: &mut bool,
    settings: &mut Settings,
    settings_path: Option<&Path>,
) -> Vec<ToolbarAction> {
    settings_window_full(ctx, open, settings, settings_path, None, None)
}

/// Settings window with optional `--portable` override path, build/commit display string (R6),
/// and auto-updater status/actions (R7).
pub fn settings_window_full(
    ctx: &egui::Context,
    open: &mut bool,
    settings: &mut Settings,
    settings_path: Option<&Path>,
    build_display: Option<&str>,
    update_state: Option<&UpdateUiState>,
) -> Vec<ToolbarAction> {
    let tab_id = egui::Id::unique("veloxdesk-settings-profile-tab");
    if !*open {
        ctx.data_mut(|d| d.remove::<Mode>(tab_id));
        return Vec::new();
    }

    let mut out = Vec::new();
    let before = settings.clone();
    let mut editing_mode: Mode = ctx.data_mut(|d| d.get_temp(tab_id).unwrap_or(settings.mode));
    let lang = settings.language;
    let t = tr(lang);

    egui::Window::new(t.settings_title)
        .id(egui::Id::unique("veloxdesk-settings"))
        .open(open)
        .resizable(false)
        .collapsible(false)
        .show(ctx, |ui| {
            egui::Grid::new("veloxdesk-settings")
                .num_columns(2)
                .spacing([16.0, 8.0])
                .show(ui, |ui| {
                    ui.label(t.settings_language_label).on_hover_text(t.tip_language);
                    ui.horizontal(|ui| {
                        for l in Language::ALL {
                            ui.selectable_value(&mut settings.language, l, l.label())
                                .on_hover_text(t.tip_language);
                        }
                    });
                    ui.end_row();

                    ui.label(t.settings_default_mode).on_hover_text(t.tip_default_mode);
                    ui.horizontal(|ui| {
                        let d_changed = ui
                            .selectable_value(&mut settings.mode, Mode::Desktop, Mode::Desktop.label_for(lang))
                            .on_hover_text(t.tip_default_mode)
                            .changed();
                        let g_changed = ui
                            .selectable_value(&mut settings.mode, Mode::Gaming, Mode::Gaming.label_for(lang))
                            .on_hover_text(t.tip_default_mode)
                            .changed();
                        if d_changed || g_changed {
                            editing_mode = settings.mode;
                        }
                    });
                    ui.end_row();

                    ui.label(t.settings_editing_profile).on_hover_text(t.tip_editing_profile);
                    ui.horizontal(|ui| {
                        for m in [Mode::Desktop, Mode::Gaming] {
                            ui.selectable_value(&mut editing_mode, m, m.label_for(lang))
                                .on_hover_text(t.tip_editing_profile);
                        }
                    });
                    ui.end_row();

                    let prof = settings.profile_mut(editing_mode);

                    ui.label(t.settings_quality).on_hover_text(t.tip_quality);
                    egui::ComboBox::from_id_salt("set-quality")
                        .selected_text(prof.quality.label_for(lang))
                        .show_ui(ui, |ui| {
                            for q in Quality::ALL {
                                ui.selectable_value(&mut prof.quality, q, q.label_for(lang));
                            }
                        })
                        .response
                        .on_hover_text(t.tip_quality);
                    ui.end_row();

                    ui.label(t.settings_target_fps).on_hover_text(t.tip_target_fps);
                    egui::ComboBox::from_id_salt("set-fps")
                        .selected_text(prof.fps_cap.label_for(lang))
                        .show_ui(ui, |ui| {
                            for f in FpsCap::ALL {
                                ui.selectable_value(&mut prof.fps_cap, f, f.label_for(lang));
                            }
                        })
                        .response
                        .on_hover_text(t.tip_target_fps);
                    ui.end_row();

                    ui.label(t.settings_color_hdr).on_hover_text(t.tip_color_hdr);
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut prof.hdr_10bit, t.settings_hdr_10bit)
                            .on_hover_text(t.tip_hdr_10bit);
                        ui.checkbox(&mut prof.yuv444, t.settings_yuv444)
                            .on_hover_text(t.tip_yuv444);
                    });
                    ui.end_row();

                    ui.label(t.settings_audio).on_hover_text(t.tip_audio);
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut prof.audio_enabled, t.settings_remote_audio)
                            .on_hover_text(t.tip_remote_audio);
                        ui.checkbox(&mut prof.mic_enabled, t.settings_microphone)
                            .on_hover_text(t.tip_microphone);
                    });
                    ui.end_row();

                    ui.label(t.settings_presentation).on_hover_text(t.tip_presentation);
                    egui::ComboBox::from_id_salt("set-present")
                        .selected_text(prof.present.label_for(lang))
                        .show_ui(ui, |ui| {
                            for p in Present::ALL {
                                ui.selectable_value(&mut prof.present, p, p.label_for(lang));
                            }
                        })
                        .response
                        .on_hover_text(t.tip_presentation);
                    ui.end_row();

                    ui.label(t.settings_scaling).on_hover_text(t.tip_scaling);
                    egui::ComboBox::from_id_salt("set-scale")
                        .selected_text(prof.scale.label_for(lang))
                        .show_ui(ui, |ui| {
                            for s in Scale::ALL {
                                ui.selectable_value(&mut prof.scale, s, s.label_for(lang));
                            }
                        })
                        .response
                        .on_hover_text(t.tip_scaling);
                    ui.end_row();

                    ui.label(t.settings_perf_hud).on_hover_text(t.tip_perf_hud);
                    ui.checkbox(&mut prof.show_hud, t.settings_hud_show_on_connect)
                        .on_hover_text(t.tip_perf_hud);
                    ui.end_row();

                    ui.label(t.settings_decoder).on_hover_text(t.tip_decoder);
                    ui.checkbox(&mut prof.prefer_hw_decode, t.settings_prefer_hw_decode)
                        .on_hover_text(t.tip_decoder);
                    ui.end_row();

                    ui.label(t.settings_gaming_mouse).on_hover_text(t.tip_gaming_mouse);
                    ui.checkbox(&mut prof.relative_mouse_in_gaming, t.settings_relative_mouse)
                        .on_hover_text(t.tip_gaming_mouse);
                    ui.end_row();

                    ui.label(t.settings_clipboard).on_hover_text(t.tip_clipboard);
                    ui.checkbox(&mut prof.clipboard_sync, t.settings_clipboard_sync)
                        .on_hover_text(t.tip_clipboard);
                    ui.end_row();

                    ui.label(t.settings_rendezvous_server).on_hover_text(t.tip_rendezvous_server);
                    ui.add(
                        egui::TextEdit::singleline(&mut settings.rendezvous_server)
                            .hint_text("127.0.0.1:21116")
                            .desired_width(200.0),
                    )
                    .on_hover_text(t.tip_rendezvous_server);
                    ui.end_row();

                    let version_text = build_display
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("v{}", env!("CARGO_PKG_VERSION")));
                    ui.label(t.settings_version_label).on_hover_text(t.tip_version);
                    ui.label(egui::RichText::new(version_text).monospace())
                        .on_hover_text(t.tip_version);
                    ui.end_row();

                    ui.label(t.settings_updates_label).on_hover_text(t.tip_updates);
                    ui.horizontal(|ui| {
                        let checking = matches!(
                            update_state,
                            Some(UpdateUiState::Checking | UpdateUiState::Downloading { .. })
                        );
                        if ui
                            .add_enabled(!checking, egui::Button::new(t.update_check_btn))
                            .on_hover_text(t.tip_updates)
                            .clicked()
                        {
                            out.push(ToolbarAction::CheckForUpdates);
                        }
                        if let Some(st) = update_state {
                            match st {
                                UpdateUiState::Idle => {}
                                UpdateUiState::Checking => {
                                    ui.spinner();
                                    ui.weak(t.update_checking);
                                }
                                UpdateUiState::UpToDate => {
                                    ui.colored_label(
                                        egui::Color32::from_rgb(130, 225, 160),
                                        t.update_up_to_date,
                                    );
                                }
                                UpdateUiState::Available { latest, .. } => {
                                    ui.colored_label(
                                        egui::Color32::from_rgb(130, 210, 255),
                                        t.fmt_update_available_short(latest),
                                    );
                                    if ui.button(t.update_now_btn).on_hover_text(t.tip_updates).clicked() {
                                        out.push(ToolbarAction::StartUpdate);
                                    }
                                }
                                UpdateUiState::Downloading { status, .. } => {
                                    ui.spinner();
                                    ui.weak(if status.is_empty() { t.update_downloading } else { status.as_str() });
                                }
                                UpdateUiState::Error(e) => {
                                    ui.colored_label(
                                        egui::Color32::from_rgb(255, 120, 120),
                                        t.fmt_update_error(e),
                                    );
                                }
                            }
                        }
                    });
                    ui.end_row();
                });
        });

    if *open {
        ctx.data_mut(|d| d.insert_temp(tab_id, editing_mode));
    } else {
        ctx.data_mut(|d| d.remove::<Mode>(tab_id));
    }

    if *settings != before {
        if settings.language != before.language {
            out.push(ToolbarAction::SetLanguage(settings.language));
        }
        if settings.mode != before.mode {
            out.push(ToolbarAction::SetMode(settings.mode));
        } else {
            let cur = settings.active();
            let prev = before.active();
            if cur.present != prev.present {
                out.push(ToolbarAction::SetPresent(cur.present));
            }
            if cur.scale != prev.scale {
                out.push(ToolbarAction::SetScale(cur.scale));
            }
            if cur.quality != prev.quality {
                out.push(ToolbarAction::SetQuality(cur.quality));
            }
            if cur.fps_cap != prev.fps_cap {
                out.push(ToolbarAction::SetFpsCap(cur.fps_cap));
            }
            if cur.audio_enabled != prev.audio_enabled {
                out.push(ToolbarAction::SetAudio(cur.audio_enabled));
            }
            if cur.mic_enabled != prev.mic_enabled {
                out.push(ToolbarAction::SetMic(cur.mic_enabled));
            }
            if cur.clipboard_sync != prev.clipboard_sync {
                out.push(ToolbarAction::SetClipboardSync(cur.clipboard_sync));
            }
            if cur.show_hud != prev.show_hud {
                out.push(ToolbarAction::SetHud(cur.show_hud));
            }
        }
        settings.save_to_or_default(settings_path);
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

    fn find_text_rect(shape: &egui::epaint::Shape, needle: &str) -> Option<egui::Rect> {
        match shape {
            egui::epaint::Shape::Vec(v) => v.iter().find_map(|s| find_text_rect(s, needle)),
            egui::epaint::Shape::Text(ts) => {
                if ts.galley.text().contains(needle) {
                    Some(ts.galley.rect.translate(ts.pos.to_vec2()))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn find_all_text_rects(shape: &egui::epaint::Shape, needle: &str, out: &mut Vec<egui::Rect>) {
        match shape {
            egui::epaint::Shape::Vec(v) => {
                for s in v {
                    find_all_text_rects(s, needle, out);
                }
            }
            egui::epaint::Shape::Text(ts) if ts.galley.text().contains(needle) => {
                out.push(ts.galley.rect.translate(ts.pos.to_vec2()));
            }
            _ => {}
        }
    }

    fn shapes_contain_text(shapes: &[egui::epaint::ClippedShape], needle: &str) -> bool {
        shapes.iter().any(|c| find_text_rect(&c.shape, needle).is_some())
    }

    fn click_at(ctx: &egui::Context, pos: egui::Pos2, mut f: impl FnMut(&egui::Context)) {
        for pressed in [true, false] {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0))),
                events: vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| f(ui.ctx()));
            out.textures_delta.clear();
        }
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
            pointer_locked: true,
            active_gamepads: 2,
            privacy_mode: false,
            files_open: false,
            terminal_open: false,
            tunnels_open: false,
        };
        let mut screen = crate::connect::ConnectScreen::new(&settings);
        let mut open = true;
        run(|ui| {
            let a = tb.show(ui, &info, &mut settings);
            assert!(a.is_empty());
            let _ = screen.show(ui, &mut settings, false);
            let a = settings_window(ui.ctx(), &mut open, &mut Settings::default());
            assert!(a.is_empty());
        });
        assert_eq!(display_label(&info.displays[0]), "A (1920x1080, primary)");
    }

    #[test]
    fn toolbar_edits_active_profile_without_mutating_other_profile() {
        let ctx = egui::Context::default();
        let info = SessionInfo::default();
        let mut settings = Settings { mode: Mode::Gaming, ..Default::default() };
        let desktop_before = settings.desktop;
        let mut tb = SessionToolbar { expanded: true };

        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0))),
                ..Default::default()
            },
            |ui| {
                let _ = tb.show(ui, &info, &mut settings);
            },
        );
        out.textures_delta.clear();
        out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0))),
                ..Default::default()
            },
            |ui| {
                let _ = tb.show(ui, &info, &mut settings);
            },
        );
        out.textures_delta.clear();
        let audio_pos = out
            .shapes
            .iter()
            .find_map(|c| find_text_rect(&c.shape, "Audio"))
            .expect("Audio button in toolbar")
            .center();

        let mut emitted = Vec::new();
        for pressed in [true, false] {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0))),
                events: vec![
                    egui::Event::PointerMoved(audio_pos),
                    egui::Event::PointerButton {
                        pos: audio_pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                ..Default::default()
            };
            let mut out_click = ctx.run_ui(input, |ui| {
                emitted = tb.show(ui, &info, &mut settings);
            });
            out_click.textures_delta.clear();
        }
        assert_eq!(emitted, vec![ToolbarAction::SetAudio(false)]);
        assert!(!settings.gaming.audio_enabled);
        assert_eq!(settings.desktop, desktop_before);
    }

    #[test]
    fn settings_window_profile_tabs_edit_independently_and_respect_portable_path() {
        let dir = std::env::temp_dir().join(format!("veloxdesk-settings-tabs-{}", std::process::id()));
        let path = dir.join("veloxdesk-settings.json");
        let ctx = egui::Context::default();
        let mut settings = Settings { mode: Mode::Desktop, ..Default::default() };
        let mut open = true;

        let make_input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0))),
            ..Default::default()
        };
        let mut out = ctx.run_ui(make_input(), |ui| {
            let _ = settings_window_at(ui.ctx(), &mut open, &mut settings, Some(&path));
        });
        out.textures_delta.clear();
        out = ctx.run_ui(make_input(), |ui| {
            let _ = settings_window_at(ui.ctx(), &mut open, &mut settings, Some(&path));
        });
        out.textures_delta.clear();

        let edit_row = out
            .shapes
            .iter()
            .find_map(|c| find_text_rect(&c.shape, "Editing profile"))
            .expect("Editing profile label");
        let mut gaming_rects = Vec::new();
        for c in &out.shapes {
            find_all_text_rects(&c.shape, "Gaming", &mut gaming_rects);
        }
        let gaming_tab = gaming_rects
            .into_iter()
            .find(|r| (r.center().y - edit_row.center().y).abs() < 16.0)
            .expect("Gaming profile tab on Editing profile row")
            .center();

        let mut tab_actions = Vec::new();
        click_at(&ctx, gaming_tab, |c| {
            tab_actions = settings_window_at(c, &mut open, &mut settings, Some(&path));
        });
        assert!(tab_actions.is_empty());
        assert_eq!(settings.mode, Mode::Desktop);

        let mut out2 = ctx.run_ui(make_input(), |ui| {
            let _ = settings_window_at(ui.ctx(), &mut open, &mut settings, Some(&path));
        });
        out2.textures_delta.clear();
        let hdr_pos = out2
            .shapes
            .iter()
            .find_map(|c| find_text_rect(&c.shape, "10-bit HDR"))
            .expect("10-bit HDR checkbox")
            .center();

        let mut edit_actions = Vec::new();
        click_at(&ctx, hdr_pos, |c| {
            edit_actions = settings_window_at(c, &mut open, &mut settings, Some(&path));
        });
        assert!(edit_actions.is_empty(), "editing inactive profile must not emit live ToolbarActions");
        assert!(settings.gaming.hdr_10bit);
        assert!(!settings.desktop.hdr_10bit);
        let reloaded = Settings::load(&path).unwrap();
        assert!(reloaded.gaming.hdr_10bit);
        assert!(!reloaded.desktop.hdr_10bit);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn settings_window_switching_default_mode_emits_set_mode() {
        let dir = std::env::temp_dir().join(format!("veloxdesk-settings-mode-{}", std::process::id()));
        let path = dir.join("veloxdesk-settings.json");
        let ctx = egui::Context::default();
        let mut settings = Settings { mode: Mode::Desktop, ..Default::default() };
        let mut open = true;

        let make_input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0))),
            ..Default::default()
        };
        let mut out0 = ctx.run_ui(make_input(), |ui| {
            let _ = settings_window_at(ui.ctx(), &mut open, &mut settings, Some(&path));
        });
        out0.textures_delta.clear();
        let mut out = ctx.run_ui(make_input(), |ui| {
            let _ = settings_window_at(ui.ctx(), &mut open, &mut settings, Some(&path));
        });
        out.textures_delta.clear();

        let default_row = out
            .shapes
            .iter()
            .find_map(|c| find_text_rect(&c.shape, "Default mode"))
            .expect("Default mode label");
        let mut gaming_rects = Vec::new();
        for c in &out.shapes {
            find_all_text_rects(&c.shape, "Gaming", &mut gaming_rects);
        }
        let gaming_btn = gaming_rects
            .into_iter()
            .find(|r| (r.center().y - default_row.center().y).abs() < 16.0)
            .expect("Gaming button on Default mode row")
            .center();

        let mut emitted = Vec::new();
        click_at(&ctx, gaming_btn, |c| {
            emitted = settings_window_at(c, &mut open, &mut settings, Some(&path));
        });
        assert_eq!(emitted, vec![ToolbarAction::SetMode(Mode::Gaming)]);
        assert_eq!(settings.mode, Mode::Gaming);
        assert_eq!(Settings::load(&path).unwrap().mode, Mode::Gaming);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn settings_tooltips_catalog_complete_en_and_ru() {
        let ctx = egui::Context::default();
        let mut init_out = ctx.run_ui(egui::RawInput::default(), |_| {});
        init_out.textures_delta.clear();
        let en = tr(Language::En);
        let ru = tr(Language::Ru);

        let pairs = [
            ("tip_language", en.tip_language, ru.tip_language),
            ("tip_default_mode", en.tip_default_mode, ru.tip_default_mode),
            ("tip_editing_profile", en.tip_editing_profile, ru.tip_editing_profile),
            ("tip_quality", en.tip_quality, ru.tip_quality),
            ("tip_target_fps", en.tip_target_fps, ru.tip_target_fps),
            ("tip_color_hdr", en.tip_color_hdr, ru.tip_color_hdr),
            ("tip_hdr_10bit", en.tip_hdr_10bit, ru.tip_hdr_10bit),
            ("tip_yuv444", en.tip_yuv444, ru.tip_yuv444),
            ("tip_audio", en.tip_audio, ru.tip_audio),
            ("tip_remote_audio", en.tip_remote_audio, ru.tip_remote_audio),
            ("tip_microphone", en.tip_microphone, ru.tip_microphone),
            ("tip_presentation", en.tip_presentation, ru.tip_presentation),
            ("tip_scaling", en.tip_scaling, ru.tip_scaling),
            ("tip_perf_hud", en.tip_perf_hud, ru.tip_perf_hud),
            ("tip_decoder", en.tip_decoder, ru.tip_decoder),
            ("tip_gaming_mouse", en.tip_gaming_mouse, ru.tip_gaming_mouse),
            ("tip_clipboard", en.tip_clipboard, ru.tip_clipboard),
            ("tip_rendezvous_server", en.tip_rendezvous_server, ru.tip_rendezvous_server),
        ];
        for (name, e, r) in pairs {
            assert!(!e.trim().is_empty(), "{name} EN empty");
            assert!(!r.trim().is_empty(), "{name} RU empty");
            assert_ne!(e, r, "{name} EN and RU must differ");
            assert!(
                r.chars().any(|c| ('\u{0400}'..='\u{04FF}').contains(&c)),
                "{name} RU must contain Cyrillic: {r:?}"
            );
            assert!(ctx.fonts_mut(|f| f.has_glyphs(&egui::FontId::proportional(14.0), r)));
        }
    }

    #[test]
    fn settings_window_hover_delay_shows_tooltips_in_en_and_ru() {
        fn run_settings_frame(
            ctx: &egui::Context,
            time: f64,
            events: Vec<egui::Event>,
            settings: &mut Settings,
            open: &mut bool,
        ) -> egui::FullOutput {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0))),
                time: Some(time),
                events,
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| {
                let _ = settings_window_at(ui.ctx(), open, settings, None);
            });
            out.textures_delta.clear();
            out
        }

        for lang in Language::ALL {
            let t = tr(lang);
            let targets: Vec<(&str, &str)> = vec![
                (t.settings_quality, t.tip_quality),
                (Quality::Balanced.label_for(lang), t.tip_quality),
                (t.settings_hdr_10bit, t.tip_hdr_10bit),
                (t.settings_microphone, t.tip_microphone),
                (Language::Ru.label(), t.tip_language),
                (t.settings_rendezvous_server, t.tip_rendezvous_server),
            ];
            for (needle, expected_tip) in targets {
                let ctx = egui::Context::default();
                let mut settings = Settings { language: lang, ..Default::default() };
                let mut open = true;

                let _ = run_settings_frame(&ctx, 0.0, Vec::new(), &mut settings, &mut open);
                let warm = run_settings_frame(&ctx, 0.02, Vec::new(), &mut settings, &mut open);
                let pos = warm
                    .shapes
                    .iter()
                    .find_map(|c| find_text_rect(&c.shape, needle))
                    .unwrap_or_else(|| panic!("target {needle:?} not found for {lang:?}"))
                    .center();

                let move_out = run_settings_frame(
                    &ctx,
                    0.10,
                    vec![egui::Event::PointerMoved(pos)],
                    &mut settings,
                    &mut open,
                );
                assert!(
                    !shapes_contain_text(&move_out.shapes, expected_tip),
                    "tooltip {expected_tip:?} should not appear immediately on move"
                );

                let pre_delay = run_settings_frame(&ctx, 0.15, Vec::new(), &mut settings, &mut open);
                assert!(
                    !shapes_contain_text(&pre_delay.shapes, expected_tip),
                    "tooltip {expected_tip:?} should not appear before 0.5s hover delay"
                );
                let delay = pre_delay.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
                assert!(
                    delay <= std::time::Duration::from_secs_f32(0.55),
                    "egui must schedule repaint <= 0.55s for hover tooltip wakeup, got {delay:?}"
                );

                let _ = run_settings_frame(&ctx, 0.75, Vec::new(), &mut settings, &mut open);
                let post_delay = run_settings_frame(&ctx, 0.76, Vec::new(), &mut settings, &mut open);
                assert!(
                    shapes_contain_text(&post_delay.shapes, expected_tip),
                    "tooltip {expected_tip:?} should appear after holding cursor on {needle:?} ({lang:?})"
                );

                let leave_out = run_settings_frame(
                    &ctx,
                    0.80,
                    vec![egui::Event::PointerMoved(egui::pos2(5.0, 5.0))],
                    &mut settings,
                    &mut open,
                );
                assert!(
                    !shapes_contain_text(&leave_out.shapes, expected_tip),
                    "tooltip {expected_tip:?} should hide when pointer leaves"
                );
            }
        }
    }
}
