//! M4 Remote Terminal window (ConPTY / POSIX PTY monospace scrollback, input & resize).

use crate::i18n::tr;
use crate::settings::Language;

/// Maximum retained scrollback buffer in bytes.
const MAX_SCROLLBACK_BYTES: usize = 64 * 1024;

/// Actions emitted by [`TerminalWindow`] to the viewer session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalAction {
    Open { cols: u32, rows: u32, shell: String },
    SendInput(Vec<u8>),
    Resize { cols: u32, rows: u32 },
    Close,
}

/// Strip ANSI CSI/OSC escape sequences so raw PTY output renders cleanly in monospace egui text.
pub fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    for ch in chars.by_ref() {
                        if ('@'..='~').contains(&ch) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    while let Some(ch) = chars.next() {
                        if ch == '\x07' {
                            break;
                        }
                        if ch == '\x1b' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
        } else if c != '\r' {
            out.push(c);
        }
    }
    out
}

/// Interactive Remote Terminal egui window.
#[derive(Debug, Clone)]
pub struct TerminalWindow {
    pub open: bool,
    pub connected: bool,
    pub shell: String,
    pub cols: u32,
    pub rows: u32,
    pub output: String,
    pub input_line: String,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
}

impl Default for TerminalWindow {
    fn default() -> Self {
        Self {
            open: false,
            connected: false,
            shell: String::new(),
            cols: 100,
            rows: 30,
            output: String::new(),
            input_line: String::new(),
            exit_code: None,
            error: None,
        }
    }
}

impl TerminalWindow {
    /// Append raw bytes received from the remote PTY (UTF-8 lossy, ANSI stripped, bounded).
    pub fn append_output(&mut self, data: &[u8]) {
        let text = String::from_utf8_lossy(data);
        let cleaned = strip_ansi(&text);
        self.output.push_str(&cleaned);
        if self.output.len() > MAX_SCROLLBACK_BYTES {
            let mut cut = self.output.len() - MAX_SCROLLBACK_BYTES;
            while cut < self.output.len() && !self.output.is_char_boundary(cut) {
                cut += 1;
            }
            self.output.drain(..cut);
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Vec<TerminalAction> {
        self.show_for(ctx, Language::En)
    }

    pub fn show_for(&mut self, ctx: &egui::Context, lang: Language) -> Vec<TerminalAction> {
        let t = tr(lang);
        let mut actions = Vec::new();
        let mut open = self.open;
        egui::Window::new(t.terminal_title)
            .id(egui::Id::unique("veloxdesk-terminal"))
            .open(&mut open)
            .default_size(egui::vec2(720.0, 430.0))
            .resizable(true)
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(t.terminal_shell_label);
                    ui.add_enabled(
                        !self.connected,
                        egui::TextEdit::singleline(&mut self.shell)
                            .hint_text(t.terminal_shell_hint)
                            .desired_width(120.0),
                    );
                    if !self.connected {
                        if ui.button(t.terminal_open_btn).clicked() {
                            self.exit_code = None;
                            self.error = None;
                            actions.push(TerminalAction::Open {
                                cols: self.cols,
                                rows: self.rows,
                                shell: self.shell.trim().to_string(),
                            });
                        }
                    } else if ui.button(t.terminal_close_btn).clicked() {
                        actions.push(TerminalAction::Close);
                    }
                    ui.separator();
                    ui.label(t.terminal_size_label);
                    let c_resp = ui.add(egui::DragValue::new(&mut self.cols).range(20..=300));
                    ui.label("×");
                    let r_resp = ui.add(egui::DragValue::new(&mut self.rows).range(5..=120));
                    if self.connected && (c_resp.changed() || r_resp.changed() || ui.button(t.terminal_resize_btn).clicked())
                    {
                        actions.push(TerminalAction::Resize { cols: self.cols, rows: self.rows });
                    }
                    if self.connected && ui.button("Ctrl+C").clicked() {
                        actions.push(TerminalAction::SendInput(vec![0x03]));
                    }
                    if ui.button(t.terminal_clear_btn).clicked() {
                        self.output.clear();
                    }
                });
                ui.separator();

                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(16, 18, 22))
                    .inner_margin(egui::Margin::same(6))
                    .corner_radius(4.0)
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .max_height(280.0)
                            .stick_to_bottom(true)
                            .show(ui, |ui| {
                                ui.set_min_width(ui.available_width());
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(&self.output)
                                            .font(egui::FontId::monospace(12.5))
                                            .color(egui::Color32::from_rgb(220, 230, 220)),
                                    )
                                    .wrap(),
                                );
                            });
                    });

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let edit = ui.add_enabled(
                        self.connected,
                        egui::TextEdit::singleline(&mut self.input_line)
                            .font(egui::FontId::monospace(12.5))
                            .hint_text(t.terminal_input_hint)
                            .desired_width((ui.available_width() - 96.0).max(120.0)),
                    );
                    let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if self.connected && (ui.button(t.terminal_send_btn).clicked() || enter) {
                        let mut bytes = self.input_line.as_bytes().to_vec();
                        bytes.extend_from_slice(b"\r\n");
                        self.input_line.clear();
                        actions.push(TerminalAction::SendInput(bytes));
                    }
                });

                if let Some(code) = self.exit_code {
                    ui.weak(t.terminal_exit_code(code));
                }
                if let Some(err) = &self.error {
                    ui.colored_label(egui::Color32::from_rgb(255, 110, 110), err);
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
    fn ansi_stripping_and_scrollback_cap() {
        assert_eq!(strip_ansi("\x1b[32mhello\x1b[0m\r\nworld"), "hello\nworld");
        assert_eq!(strip_ansi("\x1b]0;title\x07ok"), "ok");
        let mut t = TerminalWindow::default();
        t.append_output(&vec![b'a'; MAX_SCROLLBACK_BYTES + 1024]);
        assert!(t.output.len() <= MAX_SCROLLBACK_BYTES);
    }

    #[test]
    fn terminal_renders_headless() {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0))),
            ..Default::default()
        };
        let mut term = TerminalWindow { open: true, connected: true, ..Default::default() };
        term.append_output(b"VELOXDESK_M4_OK\r\n");
        let mut out = ctx.run_ui(input, |ui| {
            assert!(term.show(ui.ctx()).is_empty());
        });
        out.textures_delta.clear();
    }
}
