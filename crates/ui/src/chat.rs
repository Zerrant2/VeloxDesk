//! Reusable bidirectional session chat panel for both the viewer session and the embedded host.

use veloxdesk_protocol::control::ChatMessage;

use crate::i18n::tr;
use crate::settings::Language;

/// One chat message rendered in [`ChatWindow`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatEntry {
    pub id: u64,
    pub sender_name: String,
    pub text: String,
    /// Wall-clock timestamp in milliseconds since UNIX epoch.
    pub ts_ms: u64,
    /// `true` for locally sent messages, `false` for remote peer messages.
    pub outgoing: bool,
    /// `true` once acknowledged by the remote peer via `ChatAck`.
    pub acked: bool,
}

/// Actions emitted by [`ChatWindow`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatAction {
    Send(String),
}

/// Format a UNIX epoch millisecond timestamp as `HH:MM` (UTC).
pub fn format_hhmm(ts_ms: u64) -> String {
    let secs = ts_ms / 1000;
    let day_secs = secs % 86_400;
    let hours = day_secs / 3600;
    let minutes = (day_secs % 3600) / 60;
    format!("{hours:02}:{minutes:02}")
}

/// Floating chat window usable on both Viewer and Host sides.
#[derive(Debug, Clone)]
pub struct ChatWindow {
    id_salt: &'static str,
    pub open: bool,
    pub is_host: bool,
    pub messages: Vec<ChatEntry>,
    pub input: String,
    pub unread: usize,
    pub allow_chat: bool,
    pub error: Option<String>,
}

impl Default for ChatWindow {
    fn default() -> Self {
        Self::new("veloxdesk-chat", false)
    }
}

impl ChatWindow {
    pub fn new(id_salt: &'static str, is_host: bool) -> Self {
        Self {
            id_salt,
            open: false,
            is_host,
            messages: Vec::new(),
            input: String::new(),
            unread: 0,
            allow_chat: true,
            error: None,
        }
    }

    /// Open or close the chat window; opening automatically clears the unread counter.
    pub fn set_open(&mut self, open: bool) {
        self.open = open;
        if open {
            self.unread = 0;
        }
    }

    /// Record an incoming chat message from the remote peer.
    /// Increments `unread` when the window is currently closed.
    pub fn push_incoming(&mut self, msg: ChatMessage) {
        self.messages.push(ChatEntry {
            id: msg.id,
            sender_name: msg.sender_name,
            text: msg.text,
            ts_ms: msg.ts_ms,
            outgoing: false,
            acked: true,
        });
        if !self.open {
            self.unread = self.unread.saturating_add(1);
        }
    }

    /// Record a locally sent chat message (initially unacknowledged until `ChatAck` arrives).
    pub fn push_outgoing(&mut self, id: u64, sender_name: String, text: String, ts_ms: u64) {
        self.messages.push(ChatEntry {
            id,
            sender_name,
            text,
            ts_ms,
            outgoing: true,
            acked: false,
        });
    }

    /// Mark an outgoing message as delivered when `ChatAck { id }` arrives.
    pub fn mark_acked(&mut self, id: u64) -> bool {
        if let Some(entry) = self.messages.iter_mut().rev().find(|m| m.outgoing && m.id == id) {
            entry.acked = true;
            true
        } else {
            false
        }
    }

    /// Reset chat history, input draft, unread badge, and errors.
    pub fn clear(&mut self) {
        self.messages.clear();
        self.input.clear();
        self.unread = 0;
        self.error = None;
    }

    /// Draw the chat window in English.
    pub fn show(&mut self, ctx: &egui::Context) -> Vec<ChatAction> {
        self.show_for(ctx, Language::En)
    }

    /// Draw the chat window in `lang`. Clears `unread` while open.
    pub fn show_for(&mut self, ctx: &egui::Context, lang: Language) -> Vec<ChatAction> {
        if !self.open {
            return Vec::new();
        }
        self.unread = 0;
        let t = tr(lang);
        let mut actions = Vec::new();
        let mut open = self.open;
        let title = if self.is_host { t.host_chat_title } else { t.chat_title };
        let input_id = egui::Id::unique(self.id_salt).with("input");

        egui::Window::new(title)
            .id(egui::Id::unique(self.id_salt))
            .open(&mut open)
            .default_size([360.0, 420.0])
            .min_size([280.0, 260.0])
            .resizable(true)
            .collapsible(false)
            .show(ctx, |ui| {
                let pal = crate::theme::palette_for(ui);
                let avail_h = (ui.available_height() - 48.0).max(120.0);

                egui::ScrollArea::vertical()
                    .id_salt(egui::Id::unique(self.id_salt).with("scroll"))
                    .max_height(avail_h)
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        if self.messages.is_empty() {
                            ui.add_space(16.0);
                            ui.vertical_centered(|ui| {
                                ui.weak(t.chat_empty);
                            });
                            return;
                        }
                        let max_bubble_w = (ui.available_width() * 0.82).max(160.0);
                        for entry in &self.messages {
                            ui.add_space(4.0);
                            let align = if entry.outgoing {
                                egui::Align::Max
                            } else {
                                egui::Align::Min
                            };
                            ui.with_layout(egui::Layout::top_down(align), |ui| {
                                let (fill, stroke) = if entry.outgoing {
                                    (pal.info_banner_bg, egui::Stroke::new(1.0, pal.info_banner_stroke))
                                } else {
                                    (
                                        ui.visuals().faint_bg_color,
                                        egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
                                    )
                                };
                                egui::Frame::new()
                                    .fill(fill)
                                    .stroke(stroke)
                                    .corner_radius(egui::CornerRadius::same(8))
                                    .inner_margin(egui::Margin::symmetric(10, 6))
                                    .show(ui, |ui| {
                                        ui.set_max_width(max_bubble_w);
                                        ui.horizontal(|ui| {
                                            let name = if entry.sender_name.trim().is_empty() {
                                                t.unknown_device
                                            } else {
                                                entry.sender_name.as_str()
                                            };
                                            ui.label(egui::RichText::new(name).strong().small());
                                            ui.weak(egui::RichText::new(format_hhmm(entry.ts_ms)).small());
                                            if entry.outgoing {
                                                if entry.acked {
                                                    crate::icons::check_widget(ui, 11.0, pal.success)
                                                        .on_hover_text(t.chat_delivered_tooltip);
                                                } else {
                                                    crate::icons::status_dot_widget(ui, 7.0, pal.muted)
                                                        .on_hover_text(t.chat_sending_tooltip);
                                                }
                                            }
                                        });
                                        ui.add(egui::Label::new(&entry.text).wrap());
                                    });
                            });
                        }
                    });

                ui.separator();

                if let Some(err) = &self.error {
                    ui.colored_label(pal.danger, err);
                }
                if !self.allow_chat {
                    ui.weak(t.chat_disabled_notice);
                    return;
                }

                ui.horizontal(|ui| {
                    let send_w = 78.0;
                    let edit_w = (ui.available_width() - send_w - 8.0).max(100.0);
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.input)
                            .id(input_id)
                            .hint_text(t.chat_input_hint)
                            .desired_width(edit_w),
                    );
                    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let can_send = !self.input.trim().is_empty();
                    let clicked = ui
                        .add_enabled(can_send, egui::Button::new(t.chat_send_btn).min_size(egui::vec2(send_w, 22.0)))
                        .clicked();
                    if (clicked || enter) && can_send {
                        let text = self.input.trim().to_string();
                        self.input.clear();
                        actions.push(ChatAction::Send(text));
                        if enter {
                            resp.request_focus();
                        }
                    }
                });
            });

        self.open = open;
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_hhmm_formats_utc_hours_and_minutes() {
        assert_eq!(format_hhmm(0), "00:00");
        // 13:45:30 UTC
        assert_eq!(format_hhmm((13 * 3600 + 45 * 60 + 30) * 1000), "13:45");
        // 23:59:59 UTC + 1 day
        assert_eq!(format_hhmm((86_400 + 23 * 3600 + 59 * 60 + 59) * 1000), "23:59");
    }

    #[test]
    fn unread_counter_and_ack_tracking() {
        let mut chat = ChatWindow::default();
        assert!(!chat.open);
        assert_eq!(chat.unread, 0);

        chat.push_incoming(ChatMessage {
            id: 1,
            sender_name: "Host".into(),
            text: "Hello".into(),
            ts_ms: 60_000,
        });
        assert_eq!(chat.unread, 1);

        chat.set_open(true);
        assert_eq!(chat.unread, 0);

        chat.push_incoming(ChatMessage {
            id: 2,
            sender_name: "Host".into(),
            text: "Open message".into(),
            ts_ms: 120_000,
        });
        assert_eq!(chat.unread, 0);

        chat.push_outgoing(10, "Viewer".into(), "Ping".into(), 180_000);
        assert!(!chat.messages.last().unwrap().acked);
        assert!(chat.mark_acked(10));
        assert!(chat.messages.last().unwrap().acked);
        assert!(!chat.mark_acked(999));
    }

    #[test]
    fn chat_window_renders_headless_en_and_ru() {
        let ctx = egui::Context::default();
        let make_input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0))),
            ..Default::default()
        };
        let mut chat = ChatWindow::new("test-chat", false);
        chat.set_open(true);
        chat.push_incoming(ChatMessage {
            id: 1,
            sender_name: "Host-PC".into(),
            text: "Привет / Hello!".into(),
            ts_ms: 45_000_000,
        });
        chat.push_outgoing(2, "Me".into(), "Acked message".into(), 45_060_000);
        chat.mark_acked(2);
        chat.push_outgoing(3, "Me".into(), "Pending message".into(), 45_120_000);

        for lang in Language::ALL {
            let mut out = ctx.run_ui(make_input(), |ui| {
                let actions = chat.show_for(ui.ctx(), lang);
                assert!(actions.is_empty());
            });
            out.textures_delta.clear();
        }
    }
}
