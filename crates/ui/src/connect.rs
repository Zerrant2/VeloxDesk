//! Connect screen: address field + recent connections.

use std::net::{SocketAddr, ToSocketAddrs};
use std::path::PathBuf;

use veloxdesk_protocol::{DEFAULT_DIRECT_PORT, DEFAULT_RENDEZVOUS_PORT};

use crate::i18n::tr;
use crate::settings::{Language, Mode, PasswordMode, Settings};

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

/// Parse `ip`, `ip:port`, `[v6]:port`, `host[:port]` with `default_port` and localized errors.
pub fn parse_host_port_i18n(input: &str, default_port: u16, lang: Language) -> Result<SocketAddr, String> {
    let t = tr(lang);
    let s = input.trim();
    if s.is_empty() {
        return Err(t.err_enter_address.into());
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
        .map_err(|e| t.err_cannot_resolve(s, &e))?
        .next()
        .ok_or_else(|| t.err_no_address(s))
}

/// Parse `ip`, `ip:port`, `[v6]:port`, `host[:port]` with `default_port`.
pub fn parse_host_port(input: &str, default_port: u16) -> Result<SocketAddr, String> {
    parse_host_port_i18n(input, default_port, Language::En)
}

/// Parse `ip`, `ip:port`, `[v6]:port`, `host[:port]` (default port 21118) with localized errors.
pub fn parse_address_i18n(input: &str, lang: Language) -> Result<SocketAddr, String> {
    parse_host_port_i18n(input, DEFAULT_DIRECT_PORT, lang)
}

/// Parse `ip`, `ip:port`, `[v6]:port`, `host[:port]` (default port 21118).
pub fn parse_address(input: &str) -> Result<SocketAddr, String> {
    parse_address_i18n(input, Language::En)
}

/// Parse a rendezvous server address (default port 21116) with localized errors.
pub fn parse_rendezvous_addr_i18n(input: &str, lang: Language) -> Result<SocketAddr, String> {
    parse_host_port_i18n(input, DEFAULT_RENDEZVOUS_PORT, lang)
}

/// Parse a rendezvous server address (default port 21116).
pub fn parse_rendezvous_addr(input: &str) -> Result<SocketAddr, String> {
    parse_rendezvous_addr_i18n(input, Language::En)
}

/// Target selected by the user: either a 9-digit Device ID (P2P / relay) or a direct socket address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectTarget {
    Direct(SocketAddr),
    DeviceId(String),
}

/// Parse either a 9-digit Device ID (with optional spaces/dashes) OR a direct `ip[:port]` / `host[:port]`
/// with localized errors.
pub fn parse_target_i18n(input: &str, lang: Language) -> Result<ConnectTarget, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err(tr(lang).err_enter_target.into());
    }
    if let Some(id) = parse_device_id(s) {
        return Ok(ConnectTarget::DeviceId(id));
    }
    parse_address_i18n(s, lang).map(ConnectTarget::Direct)
}

/// Parse either a 9-digit Device ID (with optional spaces/dashes) OR a direct `ip[:port]` / `host[:port]`.
pub fn parse_target(input: &str) -> Result<ConnectTarget, String> {
    parse_target_i18n(input, Language::En)
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

/// Pure-UI auto-update state displayed on [`ConnectScreen`] and in Settings (R7).
#[derive(Debug, Clone, Default, PartialEq)]
pub enum UpdateUiState {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available {
        current: String,
        latest: String,
        notes: String,
        html_url: String,
    },
    Downloading {
        progress: Option<f32>,
        status: String,
    },
    Error(String),
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
    /// Exit borderless fullscreen and restore normal window controls (failsafe on Connect screen).
    ExitFullscreen,
    /// Switch interface language from the Connect screen header.
    SetLanguage(Language),
    /// Download, stage, replace executable, and relaunch into the available release.
    StartUpdate,
    /// Hide the update banner for the current run.
    DismissUpdate,
    /// Trigger an on-demand GitHub Releases update check.
    CheckForUpdates,
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
    /// Optional settings file override (e.g. `--portable`), set by `App::new`.
    pub settings_path: Option<PathBuf>,
    /// Formatted version + build/commit ID shown in the header badge (e.g. `v0.2.0 (abc1234)`).
    pub build_display: String,
    /// Auto-updater UI state (R7).
    pub update_state: UpdateUiState,
    /// Whether the user clicked "Later" / "Dismiss" on the current update banner.
    pub update_dismissed: bool,
}

impl ConnectScreen {
    pub fn new(settings: &Settings) -> Self {
        Self {
            address: settings.recent.first().cloned().unwrap_or_default(),
            host_status: tr(settings.language).host_stopped.into(),
            build_display: format!("v{}", env!("CARGO_PKG_VERSION")),
            ..Default::default()
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, settings: &mut Settings, is_fullscreen: bool) -> Option<ConnectAction> {
        let mut action = None;
        let mut t = tr(settings.language);
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let before_lang = settings.language;
                for lang in Language::ALL {
                    ui.selectable_value(&mut settings.language, lang, lang.short_label())
                        .on_hover_text(t.lang_switch_tooltip);
                }
                if settings.language != before_lang {
                    settings.save_to_or_default(self.settings_path.as_deref());
                    t = tr(settings.language);
                    action = Some(ConnectAction::SetLanguage(settings.language));
                }
            });
            ui.add_space(4.0);
            let build_label = if self.build_display.is_empty() {
                format!("v{}", env!("CARGO_PKG_VERSION"))
            } else {
                self.build_display.clone()
            };
            ui.vertical_centered(|ui| {
                ui.heading(egui::RichText::new(t.app_title).size(30.0).strong());
                let badge = if settings.rendezvous_server.trim().is_empty() {
                    build_label
                } else {
                    t.build_with_rendezvous(&build_label, settings.rendezvous_server.trim())
                };
                ui.weak(badge);
            });
            ui.add_space(12.0);

            let max_w = ui.available_width().min(860.0);
            let side_pad = ((ui.available_width() - max_w) * 0.5).max(8.0);
            ui.horizontal(|ui| {
                ui.add_space(side_pad);
                ui.allocate_ui_with_layout(
                    egui::vec2(max_w, ui.available_height()),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        self.show_update_banner(ui, settings.language, &mut action);
                        ui.columns(2, |cols| {
                            self.show_this_desk(&mut cols[0], settings, &mut action);
                            self.show_remote_desk(&mut cols[1], settings, &mut action);
                        });
                    },
                );
            });
        });
        if is_fullscreen {
            egui::Area::new(egui::Id::unique("veloxdesk-exit-fullscreen"))
                .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-16.0, 12.0))
                .order(egui::Order::Foreground)
                .show(ui.ctx(), |ui| {
                    if ui
                        .button(t.exit_fullscreen_btn)
                        .on_hover_text(t.exit_fullscreen_tooltip)
                        .clicked()
                    {
                        action = Some(ConnectAction::ExitFullscreen);
                    }
                });
        }
        action
    }

    fn show_update_banner(&self, ui: &mut egui::Ui, lang: Language, action: &mut Option<ConnectAction>) {
        let t = tr(lang);
        match &self.update_state {
            UpdateUiState::Available { current, latest, notes, html_url } if !self.update_dismissed => {
                egui::Frame::group(ui.style())
                    .fill(egui::Color32::from_rgb(28, 52, 74))
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(80, 155, 220)))
                    .inner_margin(egui::Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                egui::RichText::new(t.fmt_update_available(latest, current))
                                    .strong()
                                    .color(egui::Color32::from_rgb(185, 230, 255)),
                            );
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button(t.update_later_btn).clicked() {
                                    *action = Some(ConnectAction::DismissUpdate);
                                }
                                if !html_url.is_empty() && ui.button(t.update_release_notes_btn).clicked() {
                                    *action = Some(ConnectAction::OpenUrl(html_url.clone()));
                                }
                                let btn = egui::Button::new(
                                    egui::RichText::new(t.update_now_btn)
                                        .strong()
                                        .color(egui::Color32::from_rgb(130, 235, 150)),
                                );
                                if ui.add(btn).clicked() {
                                    *action = Some(ConnectAction::StartUpdate);
                                }
                            });
                        });
                        if let Some(first_line) = notes.lines().map(str::trim).find(|l| !l.is_empty()) {
                            let summary: String = first_line.chars().take(120).collect();
                            ui.weak(summary);
                        }
                    });
                ui.add_space(8.0);
            }
            UpdateUiState::Downloading { progress, status } => {
                egui::Frame::group(ui.style())
                    .fill(egui::Color32::from_rgb(28, 52, 74))
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(80, 155, 220)))
                    .inner_margin(egui::Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.spinner();
                            let label = if status.is_empty() { t.update_downloading } else { status.as_str() };
                            ui.label(label);
                        });
                        if let Some(p) = progress {
                            ui.add_space(4.0);
                            ui.add(egui::ProgressBar::new(p.clamp(0.0, 1.0)).show_percentage());
                        }
                    });
                ui.add_space(8.0);
            }
            UpdateUiState::Error(err) if !self.update_dismissed => {
                egui::Frame::group(ui.style())
                    .fill(egui::Color32::from_rgb(68, 28, 28))
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(210, 95, 95)))
                    .inner_margin(egui::Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.horizontal_wrapped(|ui| {
                            ui.colored_label(
                                egui::Color32::from_rgb(255, 145, 145),
                                t.fmt_update_error(err),
                            );
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button(t.update_dismiss_btn).clicked() {
                                    *action = Some(ConnectAction::DismissUpdate);
                                }
                                if ui.button(t.update_retry_btn).clicked() {
                                    *action = Some(ConnectAction::CheckForUpdates);
                                }
                            });
                        });
                    });
                ui.add_space(8.0);
            }
            _ => {}
        }
    }

    fn show_this_desk(&mut self, ui: &mut egui::Ui, settings: &mut Settings, action: &mut Option<ConnectAction>) {
        let t = tr(settings.language);
        egui::Frame::group(ui.style()).inner_margin(egui::Margin::same(14)).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(t.this_desk_heading).size(18.0).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (dot_color, pill) = if self.active_sessions > 0 {
                        (
                            egui::Color32::from_rgb(100, 190, 255),
                            t.active_sessions_pill(self.active_sessions),
                        )
                    } else if self.host_running {
                        (egui::Color32::from_rgb(110, 230, 140), t.status_ready.into())
                    } else {
                        (egui::Color32::from_rgb(150, 150, 160), t.status_offline.into())
                    };
                    // Right-to-left places the first widget on the far right, so emit the pill
                    // first and the status dot second to paint `[dot] [pill]` right-aligned.
                    ui.spacing_mut().item_spacing.x = 5.0;
                    ui.colored_label(dot_color, pill);
                    crate::icons::status_dot_widget(ui, 10.0, dot_color);
                });
            });
            ui.weak(t.this_desk_subtitle);
            ui.separator();

            // Device ID block
            ui.label(egui::RichText::new(t.device_id_label).small().weak());
            let formatted_id = if self.device_id.is_empty() {
                t.unavailable.to_string()
            } else {
                format_device_id(&self.device_id)
            };
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&formatted_id).monospace().size(22.0).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(!self.device_id.is_empty(), egui::Button::new(t.copy_id_btn))
                        .clicked()
                    {
                        self.copied_notice = Some(t.device_id_copied.into());
                        *action = Some(ConnectAction::CopyText(formatted_id.clone()));
                    }
                });
            });

            if self.tailscale.running
                && let Some(ts_ip) = self.tailscale.self_ip.clone()
            {
                ui.add_space(6.0);
                ui.label(egui::RichText::new(t.tailscale_ip_label).small().weak());
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(&ts_ip).monospace().size(15.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(t.copy_btn).clicked() {
                            self.copied_notice = Some(t.tailscale_ip_copied.into());
                            *action = Some(ConnectAction::CopyText(ts_ip.clone()));
                        }
                    });
                });
            }

            ui.add_space(10.0);

            // Password / OTP block (reflects PasswordMode: OneTime, Permanent, Both)
            let perm_only_active =
                settings.password_mode == PasswordMode::Permanent && settings.has_permanent_password();
            if perm_only_active {
                ui.label(egui::RichText::new(t.settings_permanent_password).small().weak());
                ui.colored_label(
                    egui::Color32::from_rgb(130, 225, 160),
                    egui::RichText::new(t.connect_permanent_active).strong(),
                );
            } else {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(t.otp_label).small().weak());
                    if settings.password_mode == PasswordMode::Both && settings.has_permanent_password() {
                        ui.colored_label(
                            egui::Color32::from_rgb(130, 210, 255),
                            egui::RichText::new(t.connect_plus_permanent_badge).small().strong(),
                        );
                    }
                });
                let otp_display = if self.host_otp.is_empty() {
                    "—".to_string()
                } else if self.show_otp {
                    self.host_otp.clone()
                } else {
                    "•".repeat(self.host_otp.len().max(8))
                };
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new(&otp_display).monospace().size(18.0));
                    let has_otp = !self.host_otp.is_empty();
                    if ui
                        .add_enabled(has_otp, egui::Button::new(if self.show_otp { t.hide_btn } else { t.show_btn }))
                        .clicked()
                    {
                        self.show_otp = !self.show_otp;
                    }
                    if ui.add_enabled(has_otp, egui::Button::new(t.copy_btn)).clicked() {
                        self.copied_notice = Some(t.otp_copied.into());
                        *action = Some(ConnectAction::CopyText(self.host_otp.clone()));
                    }
                    if ui
                        .add_enabled(self.host_running, egui::Button::new(t.refresh_btn))
                        .on_hover_text(t.otp_refresh_tooltip)
                        .clicked()
                    {
                        self.copied_notice = None;
                        *action = Some(ConnectAction::RotateOtp);
                    }
                });
            }

            if let Some(note) = &self.copied_notice {
                ui.colored_label(egui::Color32::from_rgb(130, 225, 160), note);
            }

            ui.add_space(10.0);
            ui.separator();

            // Status line + Start/Stop + Require Accept
            let status_text = match self.host_status.as_str() {
                "" | "Stopped" | "Остановлен" | "Listening" | "Ожидание подключений" => {
                    if self.host_running {
                        t.host_listening
                    } else {
                        t.host_stopped
                    }
                }
                other => other,
            };
            ui.label(t.host_status_line(status_text));
            if let Some(err) = &self.host_error {
                ui.colored_label(egui::Color32::from_rgb(255, 110, 110), err);
            }

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let toggle_label = if self.host_running { t.stop_hosting_btn } else { t.start_hosting_btn };
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
                .checkbox(&mut settings.require_accept, t.require_accept_label)
                .on_hover_text(t.require_accept_tooltip)
                .changed()
            {
                *action = Some(ConnectAction::SetRequireAccept(settings.require_accept));
            }
        });
    }

    fn show_remote_desk(&mut self, ui: &mut egui::Ui, settings: &mut Settings, action: &mut Option<ConnectAction>) {
        let lang = settings.language;
        let t = tr(lang);
        egui::Frame::group(ui.style()).inner_margin(egui::Margin::same(14)).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(egui::RichText::new(t.remote_desk_heading).size(18.0).strong());
            ui.weak(t.remote_desk_subtitle);
            ui.separator();

            let busy = self.status.is_some();
            let edit = ui.add_enabled(
                !busy,
                egui::TextEdit::singleline(&mut self.address)
                    .hint_text(t.remote_address_hint)
                    .desired_width(ui.available_width()),
            );
            let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let connect = ui.add_enabled(!busy, egui::Button::new(t.connect_btn).min_size(egui::vec2(110.0, 28.0)));
                if (connect.clicked() || enter) && !busy {
                    *action = self.try_connect(lang);
                }
                if ui.add(egui::Button::new(t.settings_btn).min_size(egui::vec2(96.0, 28.0))).clicked() {
                    *action = Some(ConnectAction::OpenSettings);
                }
            });

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(t.mode_colon_label);
                let changed = ui
                    .selectable_value(&mut settings.mode, Mode::Desktop, Mode::Desktop.label_for(lang))
                    .changed()
                    | ui
                        .selectable_value(&mut settings.mode, Mode::Gaming, Mode::Gaming.label_for(lang))
                        .changed();
                if changed {
                    settings.save_to_or_default(self.settings_path.as_deref());
                }
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
                ui.label(egui::RichText::new(t.recent_connections_heading).strong());
                let mut remove = None;
                for (i, addr) in settings.recent.iter().enumerate() {
                    ui.horizontal(|ui| {
                        let btn_w = (ui.available_width() - 32.0).max(120.0);
                        let resp = ui.add_enabled_ui(!busy, |ui| {
                            ui.add_sized(egui::vec2(btn_w, 22.0), egui::Button::new(addr).truncate())
                        });
                        if resp.inner.clicked() {
                            self.address = addr.clone();
                            *action = self.try_connect(lang);
                        }
                        if crate::icons::icon_small_button(
                            ui,
                            egui::vec2(22.0, 22.0),
                            crate::icons::close_cross_atom(10.0),
                        )
                        .on_hover_text(t.remove_tooltip)
                        .clicked()
                        {
                            remove = Some(i);
                        }
                    });
                }
                if let Some(i) = remove {
                    settings.recent.remove(i);
                    settings.save_to_or_default(self.settings_path.as_deref());
                }
            }

            // Tailscale Devices section (1-click zero-VPS P2P)
            ui.add_space(12.0);
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(t.tailscale_devices_heading).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .small_button(t.refresh_btn)
                        .on_hover_text(t.tailscale_refresh_tooltip)
                        .clicked()
                    {
                        *action = Some(ConnectAction::RefreshTailscale);
                    }
                });
            });

            if !self.tailscale.installed {
                ui.weak(t.tailscale_install_hint);
                ui.add_space(4.0);
                if ui.button(t.tailscale_download_btn).clicked() {
                    *action = Some(ConnectAction::OpenUrl("https://tailscale.com/download".into()));
                }
            } else if !self.tailscale.running {
                let msg = self.tailscale.error.as_deref().unwrap_or(t.tailscale_not_connected);
                ui.weak(msg);
                ui.add_space(4.0);
                if ui.button(t.tailscale_start_btn).clicked() {
                    *action = Some(ConnectAction::StartTailscale);
                }
            } else if self.tailscale.peers.is_empty() {
                ui.weak(t.tailscale_no_peers);
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
                            crate::icons::status_dot_widget(ui, 10.0, dot_color);
                            ui.label(egui::RichText::new(&peer.name).strong());
                            ui.weak(&peer.ip);
                            if !peer.os.is_empty() {
                                ui.weak(format!("({})", peer.os));
                            }
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.add_enabled(!busy, egui::Button::new(t.connect_btn).small()).clicked() {
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
                    *action = self.try_connect(lang);
                }
            }
        });
    }

    fn try_connect(&mut self, lang: Language) -> Option<ConnectAction> {
        match parse_target_i18n(&self.address, lang) {
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
            assert!(screen.show(ui, &mut settings, false).is_none());
        });
        out.textures_delta.clear();
    }

    fn find_text_center(shape: &egui::epaint::Shape, needle: &str) -> Option<egui::Pos2> {
        match shape {
            egui::epaint::Shape::Vec(v) => v.iter().find_map(|s| find_text_center(s, needle)),
            egui::epaint::Shape::Text(ts) => {
                if ts.galley.text().contains(needle) {
                    Some(ts.pos + ts.galley.rect.center().to_vec2())
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    #[test]
    fn connect_screen_fullscreen_failsafe_emits_exit_fullscreen() {
        let ctx = egui::Context::default();
        let make_input = |events: Vec<egui::Event>| egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0))),
            events,
            ..Default::default()
        };
        let mut settings = Settings::default();
        let mut screen = ConnectScreen::new(&settings);

        // When not fullscreen, failsafe button is NOT rendered.
        let mut out_windowed = ctx.run_ui(make_input(Vec::new()), |ui| {
            assert!(screen.show(ui, &mut settings, false).is_none());
        });
        out_windowed.textures_delta.clear();
        assert!(
            out_windowed.shapes.iter().all(|c| find_text_center(&c.shape, "Exit Fullscreen").is_none()),
            "failsafe button should not render when is_fullscreen is false"
        );

        // When fullscreen, failsafe button renders top-right (after Area sizing pass) and emits ExitFullscreen on click.
        for _ in 0..2 {
            let mut out = ctx.run_ui(make_input(Vec::new()), |ui| {
                assert!(screen.show(ui, &mut settings, true).is_none());
            });
            out.textures_delta.clear();
        }
        let mut out_fs = ctx.run_ui(make_input(Vec::new()), |ui| {
            assert!(screen.show(ui, &mut settings, true).is_none());
        });
        out_fs.textures_delta.clear();
        let btn_pos = out_fs
            .shapes
            .iter()
            .find_map(|c| find_text_center(&c.shape, "Exit Fullscreen"))
            .expect("Exit Fullscreen failsafe button should render when is_fullscreen is true");

        let mut out_down = ctx.run_ui(
            make_input(vec![
                egui::Event::PointerMoved(btn_pos),
                egui::Event::PointerButton {
                    pos: btn_pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ]),
            |ui| {
                let _ = screen.show(ui, &mut settings, true);
            },
        );
        out_down.textures_delta.clear();

        let mut emitted = None;
        let mut out_up = ctx.run_ui(
            make_input(vec![
                egui::Event::PointerMoved(btn_pos),
                egui::Event::PointerButton {
                    pos: btn_pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ]),
            |ui| {
                emitted = screen.show(ui, &mut settings, true);
            },
        );
        out_up.textures_delta.clear();
        assert_eq!(emitted, Some(ConnectAction::ExitFullscreen));
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

    fn collect_circles(shape: &egui::epaint::Shape, out: &mut Vec<(egui::Pos2, f32)>) {
        match shape {
            egui::epaint::Shape::Vec(v) => {
                for s in v {
                    collect_circles(s, out);
                }
            }
            egui::epaint::Shape::Circle(cs) => {
                out.push((cs.center, cs.radius));
            }
            _ => {}
        }
    }

    fn collect_line_segments(shape: &egui::epaint::Shape, out: &mut Vec<[egui::Pos2; 2]>) {
        match shape {
            egui::epaint::Shape::Vec(v) => {
                for s in v {
                    collect_line_segments(s, out);
                }
            }
            egui::epaint::Shape::LineSegment { points, .. } => {
                out.push(*points);
            }
            _ => {}
        }
    }

    #[test]
    fn this_desk_status_dot_precedes_pill_left_to_right() {
        let ctx = egui::Context::default();
        let make_input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0))),
            ..Default::default()
        };

        for (host_running, active_sessions, expected_pill) in [
            (true, 1, "1 active"),
            (true, 0, "Ready"),
            (false, 0, "Offline"),
        ] {
            let mut settings = Settings::default();
            let mut screen = ConnectScreen::new(&settings);
            screen.host_running = host_running;
            screen.active_sessions = active_sessions;

            let mut out = ctx.run_ui(make_input(), |ui| {
                let _ = screen.show(ui, &mut settings, false);
            });
            out.textures_delta.clear();

            let title_rect = out
                .shapes
                .iter()
                .find_map(|c| find_text_rect(&c.shape, "This Desk"))
                .expect("This Desk title should render");
            let pill_rect = out
                .shapes
                .iter()
                .find_map(|c| find_text_rect(&c.shape, expected_pill))
                .unwrap_or_else(|| panic!("status pill {expected_pill:?} should render"));

            let mut circles = Vec::new();
            for c in &out.shapes {
                collect_circles(&c.shape, &mut circles);
            }
            let (dot_center, dot_radius) = circles
                .into_iter()
                .find(|(center, _)| (center.y - pill_rect.center().y).abs() < 12.0)
                .unwrap_or_else(|| panic!("status_dot CircleShape should render on same row as {expected_pill:?}"));

            assert!(
                dot_center.x + dot_radius < pill_rect.min.x,
                "status_dot ({}..{}) must be strictly left of pill {expected_pill:?} ({}..{})",
                dot_center.x - dot_radius,
                dot_center.x + dot_radius,
                pill_rect.min.x,
                pill_rect.max.x
            );
            assert!(
                dot_center.x - dot_radius > title_rect.max.x + 100.0,
                "status indicator should be right-aligned away from 'This Desk' title (title max.x={}, dot min.x={})",
                title_rect.max.x,
                dot_center.x - dot_radius
            );
        }
    }

    #[test]
    fn recent_long_hostname_keeps_close_cross_in_narrow_viewport_and_removes_on_click() {
        let default_path = Settings::default_path();
        let prev_settings = default_path.as_deref().and_then(|p| std::fs::read(p).ok());
        let existed_before = default_path.as_deref().is_some_and(|p| p.exists());

        let ctx = egui::Context::default();
        let viewport = egui::vec2(700.0, 600.0);
        let make_input = |events: Vec<egui::Event>| egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, viewport)),
            events,
            ..Default::default()
        };

        let mut settings = Settings::default();
        let long_host =
            "very-long-remote-desktop-hostname-overflow-test-1234567890.internal.example.com:21118";
        settings.recent.push(long_host.into());
        let mut screen = ConnectScreen::new(&settings);

        let mut out = ctx.run_ui(make_input(Vec::new()), |ui| {
            let _ = screen.show(ui, &mut settings, false);
        });
        out.textures_delta.clear();

        let recent_heading = out
            .shapes
            .iter()
            .find_map(|c| find_text_rect(&c.shape, "Recent Connections"))
            .expect("Recent Connections heading should render");

        let mut lines = Vec::new();
        for c in &out.shapes {
            collect_line_segments(&c.shape, &mut lines);
        }
        // Filter line segments belonging to the close_cross below the Recent Connections heading.
        let cross_lines: Vec<[egui::Pos2; 2]> = lines
            .into_iter()
            .filter(|[a, b]| {
                a.y > recent_heading.max.y
                    && b.y > recent_heading.max.y
                    && (a.x - b.x).abs() > 2.0
                    && (a.y - b.y).abs() > 2.0
            })
            .collect();
        assert_eq!(
            cross_lines.len(),
            2,
            "expected 2 diagonal line segments for close_cross in narrow viewport, got {cross_lines:?}"
        );

        let min_x = cross_lines.iter().flat_map(|[a, b]| [a.x, b.x]).fold(f32::INFINITY, f32::min);
        let max_x = cross_lines.iter().flat_map(|[a, b]| [a.x, b.x]).fold(f32::NEG_INFINITY, f32::max);
        let min_y = cross_lines.iter().flat_map(|[a, b]| [a.y, b.y]).fold(f32::INFINITY, f32::min);
        let max_y = cross_lines.iter().flat_map(|[a, b]| [a.y, b.y]).fold(f32::NEG_INFINITY, f32::max);

        assert!(
            min_x >= viewport.x * 0.5 && max_x <= viewport.x - 12.0 && min_y >= 0.0 && max_y <= viewport.y,
            "close_cross ({min_x}..{max_x}, {min_y}..{max_y}) must remain inside right column and 700x600 viewport"
        );

        let cross_center = egui::pos2((min_x + max_x) * 0.5, (min_y + max_y) * 0.5);

        let mut out_down = ctx.run_ui(
            make_input(vec![
                egui::Event::PointerMoved(cross_center),
                egui::Event::PointerButton {
                    pos: cross_center,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ]),
            |ui| {
                let _ = screen.show(ui, &mut settings, false);
            },
        );
        out_down.textures_delta.clear();

        let mut out_up = ctx.run_ui(
            make_input(vec![
                egui::Event::PointerMoved(cross_center),
                egui::Event::PointerButton {
                    pos: cross_center,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ]),
            |ui| {
                let _ = screen.show(ui, &mut settings, false);
            },
        );
        out_up.textures_delta.clear();

        // Restore any pre-existing user settings file if writable.
        if let Some(p) = default_path {
            if let Some(bytes) = prev_settings {
                let _ = std::fs::write(&p, bytes);
            } else if !existed_before {
                let _ = std::fs::remove_file(&p);
            }
        }

        assert!(
            settings.recent.is_empty(),
            "clicking close_cross at {cross_center:?} should remove long hostname from settings.recent"
        );
    }

    #[test]
    fn connect_screen_language_switcher_toggles_language_and_emits_action() {
        let dir = std::env::temp_dir().join(format!("veloxdesk-connect-lang-{}", std::process::id()));
        let path = dir.join("settings.json");
        let ctx = egui::Context::default();
        let make_input = |events: Vec<egui::Event>| egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0))),
            events,
            ..Default::default()
        };
        let mut settings = Settings::default();
        let mut screen = ConnectScreen::new(&settings);
        screen.settings_path = Some(path.clone());

        let mut out0 = ctx.run_ui(make_input(Vec::new()), |ui| {
            assert!(screen.show(ui, &mut settings, false).is_none());
        });
        out0.textures_delta.clear();

        let ru_pos = out0
            .shapes
            .iter()
            .find_map(|c| find_text_center(&c.shape, "RU"))
            .expect("RU switcher button should render");

        let mut out_down = ctx.run_ui(
            make_input(vec![
                egui::Event::PointerMoved(ru_pos),
                egui::Event::PointerButton {
                    pos: ru_pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ]),
            |ui| {
                let _ = screen.show(ui, &mut settings, false);
            },
        );
        out_down.textures_delta.clear();

        let mut action = None;
        let mut out_up = ctx.run_ui(
            make_input(vec![
                egui::Event::PointerMoved(ru_pos),
                egui::Event::PointerButton {
                    pos: ru_pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ]),
            |ui| {
                action = screen.show(ui, &mut settings, false);
            },
        );
        out_up.textures_delta.clear();

        assert_eq!(action, Some(ConnectAction::SetLanguage(Language::Ru)));
        assert_eq!(settings.language, Language::Ru);
        assert_eq!(Settings::load(&path).unwrap().language, Language::Ru);

        let mut out_ru = ctx.run_ui(make_input(Vec::new()), |ui| {
            let _ = screen.show(ui, &mut settings, false);
        });
        out_ru.textures_delta.clear();
        assert!(
            out_ru.shapes.iter().any(|c| find_text_rect(&c.shape, "Это устройство").is_some()),
            "expected Russian 'Это устройство' heading after switching language"
        );
        assert!(
            out_ru.shapes.iter().any(|c| find_text_rect(&c.shape, "Удалённое управление").is_some()),
            "expected Russian 'Удалённое управление' heading after switching language"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn connect_screen_ru_narrow_viewport_fits_without_clipping() {
        let ctx = egui::Context::default();
        let viewport = egui::vec2(700.0, 600.0);
        let make_input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, viewport)),
            ..Default::default()
        };

        let mut settings = Settings {
            language: Language::Ru,
            rendezvous_server: "rdv.example.com:21116".into(),
            ..Default::default()
        };
        settings.recent.push(
            "very-long-remote-desktop-hostname-overflow-test-1234567890.internal.example.com:21118".into(),
        );
        let mut screen = ConnectScreen::new(&settings);
        screen.device_id = "123456789".into();
        screen.host_running = true;
        screen.active_sessions = 1;
        screen.host_otp = "AB12CD34".into();
        screen.tailscale = TailscaleInfo {
            installed: true,
            running: true,
            self_ip: Some("100.64.1.10".into()),
            self_name: Some("my-pc".into()),
            peers: vec![TailscalePeer {
                name: "gaming-rig".into(),
                ip: "100.64.1.20".into(),
                os: "windows".into(),
                online: true,
            }],
            error: None,
        };

        let mut out = ctx.run_ui(make_input(), |ui| {
            let _ = screen.show(ui, &mut settings, true);
        });
        out.textures_delta.clear();
        for _ in 0..2 {
            out = ctx.run_ui(make_input(), |ui| {
                let _ = screen.show(ui, &mut settings, true);
            });
            out.textures_delta.clear();
        }

        let en_rect = out.shapes.iter().find_map(|c| find_text_rect(&c.shape, "EN")).expect("EN button");
        let ru_rect = out.shapes.iter().find_map(|c| find_text_rect(&c.shape, "RU")).expect("RU button");
        let fs_rect = out
            .shapes
            .iter()
            .find_map(|c| find_text_rect(&c.shape, "Выйти из полного экрана"))
            .expect("Exit Fullscreen RU button");
        assert!(
            ru_rect.max.x + 8.0 < fs_rect.min.x || en_rect.max.y < fs_rect.min.y,
            "EN/RU switcher ({ru_rect:?}) must not overlap Exit Fullscreen ({fs_rect:?})"
        );

        let this_desk = out
            .shapes
            .iter()
            .find_map(|c| find_text_rect(&c.shape, "Это устройство"))
            .expect("Это устройство");
        let remote_desk = out
            .shapes
            .iter()
            .find_map(|c| find_text_rect(&c.shape, "Удалённое управление"))
            .expect("Удалённое управление");
        let pill_rect = out
            .shapes
            .iter()
            .find_map(|c| find_text_rect(&c.shape, "Активно: 1"))
            .expect("Активно: 1");
        let badge_rect = out
            .shapes
            .iter()
            .find_map(|c| find_text_rect(&c.shape, "Рандеву:"))
            .expect("version+rendezvous badge");

        for (name, r) in [
            ("Это устройство", this_desk),
            ("Удалённое управление", remote_desk),
            ("Выйти из полного экрана", fs_rect),
            ("Активно: 1", pill_rect),
            ("Рандеву:", badge_rect),
        ] {
            assert!(
                r.min.x >= 0.0 && r.max.x <= viewport.x && r.min.y >= 0.0 && r.max.y <= viewport.y,
                "{name} ({r:?}) clipped outside {viewport:?}"
            );
        }

        let mut circles = Vec::new();
        for c in &out.shapes {
            collect_circles(&c.shape, &mut circles);
        }
        let (dot_center, dot_radius) = circles
            .into_iter()
            .find(|(center, _)| (center.y - pill_rect.center().y).abs() < 12.0)
            .expect("status_dot on same row as Активно: 1");
        assert!(dot_center.x + dot_radius < pill_rect.min.x);

        let recent_heading = out
            .shapes
            .iter()
            .find_map(|c| find_text_rect(&c.shape, "Недавние подключения"))
            .expect("Недавние подключения");
        let mut lines = Vec::new();
        for c in &out.shapes {
            collect_line_segments(&c.shape, &mut lines);
        }
        let cross_lines: Vec<[egui::Pos2; 2]> = lines
            .into_iter()
            .filter(|[a, b]| {
                a.y > recent_heading.max.y
                    && b.y > recent_heading.max.y
                    && (a.x - b.x).abs() > 2.0
                    && (a.y - b.y).abs() > 2.0
            })
            .collect();
        assert_eq!(cross_lines.len(), 2);
        let max_x = cross_lines.iter().flat_map(|[a, b]| [a.x, b.x]).fold(f32::NEG_INFINITY, f32::max);
        assert!(max_x <= viewport.x - 12.0);
    }

    #[test]
    fn parse_i18n_ru_errors() {
        assert_eq!(
            parse_target_i18n("", Language::Ru).unwrap_err(),
            "введите 9-значный ID устройства или адрес"
        );
        assert_eq!(
            parse_host_port_i18n("", 21118, Language::Ru).unwrap_err(),
            "введите адрес"
        );
        assert!(
            crate::tunnels::parse_tunnel_spec_i18n("bad", Language::Ru)
                .unwrap_err()
                .contains("ожидалось")
        );
    }

    #[test]
    fn portable_recent_removal_saves_to_override_path_only() {
        let dir = std::env::temp_dir().join(format!("veloxdesk-portable-recent-{}", std::process::id()));
        let path = dir.join("veloxdesk-settings.json");
        let ctx = egui::Context::default();
        let viewport = egui::vec2(700.0, 600.0);
        let make_input = |events: Vec<egui::Event>| egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, viewport)),
            events,
            ..Default::default()
        };

        let mut settings = Settings::default();
        settings.recent.push("10.0.0.5:21118".into());
        let mut screen = ConnectScreen::new(&settings);
        screen.settings_path = Some(path.clone());

        let mut out = ctx.run_ui(make_input(Vec::new()), |ui| {
            let _ = screen.show(ui, &mut settings, false);
        });
        out.textures_delta.clear();

        let recent_heading = out
            .shapes
            .iter()
            .find_map(|c| find_text_rect(&c.shape, "Recent Connections"))
            .expect("Recent Connections heading");
        let mut lines = Vec::new();
        for c in &out.shapes {
            collect_line_segments(&c.shape, &mut lines);
        }
        let cross_lines: Vec<[egui::Pos2; 2]> = lines
            .into_iter()
            .filter(|[a, b]| {
                a.y > recent_heading.max.y
                    && b.y > recent_heading.max.y
                    && (a.x - b.x).abs() > 2.0
                    && (a.y - b.y).abs() > 2.0
            })
            .collect();
        assert_eq!(cross_lines.len(), 2);
        let min_x = cross_lines.iter().flat_map(|[a, b]| [a.x, b.x]).fold(f32::INFINITY, f32::min);
        let max_x = cross_lines.iter().flat_map(|[a, b]| [a.x, b.x]).fold(f32::NEG_INFINITY, f32::max);
        let min_y = cross_lines.iter().flat_map(|[a, b]| [a.y, b.y]).fold(f32::INFINITY, f32::min);
        let max_y = cross_lines.iter().flat_map(|[a, b]| [a.y, b.y]).fold(f32::NEG_INFINITY, f32::max);
        let cross_center = egui::pos2((min_x + max_x) * 0.5, (min_y + max_y) * 0.5);

        let mut out_down = ctx.run_ui(
            make_input(vec![
                egui::Event::PointerMoved(cross_center),
                egui::Event::PointerButton {
                    pos: cross_center,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ]),
            |ui| {
                let _ = screen.show(ui, &mut settings, false);
            },
        );
        out_down.textures_delta.clear();

        let mut out_up = ctx.run_ui(
            make_input(vec![
                egui::Event::PointerMoved(cross_center),
                egui::Event::PointerButton {
                    pos: cross_center,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ]),
            |ui| {
                let _ = screen.show(ui, &mut settings, false);
            },
        );
        out_up.textures_delta.clear();

        assert!(settings.recent.is_empty());
        let saved = Settings::load(&path).expect("portable settings file written");
        assert!(saved.recent.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn connect_screen_update_banner_and_build_display_render_en_and_ru() {
        let ctx = egui::Context::default();
        let make_input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0))),
            ..Default::default()
        };

        for lang in Language::ALL {
            let mut settings = Settings { language: lang, ..Default::default() };
            let mut screen = ConnectScreen::new(&settings);
            screen.build_display = "v0.2.0 (26e488f)".into();
            screen.update_state = UpdateUiState::Available {
                current: "0.2.0".into(),
                latest: "0.3.0".into(),
                notes: "Bug fixes and performance improvements".into(),
                html_url: "https://github.com/Zerrant2/VeloxDesk/releases/tag/v0.3.0".into(),
            };
            let mut out = ctx.run_ui(make_input(), |ui| {
                let _ = screen.show(ui, &mut settings, false);
            });
            out.textures_delta.clear();
            assert!(out.shapes.iter().any(|c| find_text_rect(&c.shape, "v0.2.0 (26e488f)").is_some()));
            assert!(out.shapes.iter().any(|c| find_text_rect(&c.shape, tr(lang).update_now_btn).is_some()));

            screen.update_state = UpdateUiState::Downloading {
                progress: Some(0.5),
                status: tr(lang).fmt_update_downloading_bytes(512 * 1024, Some(1024 * 1024)),
            };
            let mut out2 = ctx.run_ui(make_input(), |ui| {
                let _ = screen.show(ui, &mut settings, false);
            });
            out2.textures_delta.clear();

            screen.update_state = UpdateUiState::Error("offline".into());
            let mut out3 = ctx.run_ui(make_input(), |ui| {
                let _ = screen.show(ui, &mut settings, false);
            });
            out3.textures_delta.clear();
            assert!(out3.shapes.iter().any(|c| find_text_rect(&c.shape, tr(lang).update_retry_btn).is_some()));
        }
    }

    #[test]
    fn connect_screen_reflects_all_password_modes_en_and_ru() {
        let ctx = egui::Context::default();
        let make_input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0))),
            ..Default::default()
        };

        for lang in Language::ALL {
            let t = tr(lang);
            let mut settings = Settings {
                language: lang,
                password_mode: PasswordMode::Permanent,
                permanent_password_hash: Some("$argon2id$v=19$m=65536,t=3,p=1$salt$hash".into()),
                ..Default::default()
            };
            let mut screen = ConnectScreen::new(&settings);
            screen.host_running = true;
            screen.host_otp = "AB12CD34".into();

            // Permanent-only with hash set: shows "Permanent password active" and hides OTP Refresh.
            let mut out_perm = ctx.run_ui(make_input(), |ui| {
                let _ = screen.show(ui, &mut settings, false);
            });
            out_perm.textures_delta.clear();
            assert!(out_perm.shapes.iter().any(|c| find_text_rect(&c.shape, t.connect_permanent_active).is_some()));

            // Both with hash set: shows OTP + "+ Permanent" badge.
            settings.password_mode = PasswordMode::Both;
            let mut out_both = ctx.run_ui(make_input(), |ui| {
                let _ = screen.show(ui, &mut settings, false);
            });
            out_both.textures_delta.clear();
            assert!(out_both.shapes.iter().any(|c| find_text_rect(&c.shape, t.connect_plus_permanent_badge).is_some()));
        }
    }
}
