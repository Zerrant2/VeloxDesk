//! Resolution-independent vector icons painted directly with [`egui::Painter`].
//!
//! All helpers here are strictly decoupled from text localization so callers can
//! compose them with any translated label or tooltip.

/// Draw a filled circular status indicator centered inside `rect`.
pub fn status_dot(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let r = (rect.width().min(rect.height()) * 0.4).max(2.5);
    painter.circle_filled(rect.center(), r, color);
}

/// Draw a checkmark inside `rect` (e.g. chat delivery acknowledgement).
pub fn check(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let r = rect.shrink(rect.width().min(rect.height()) * 0.14);
    let stroke = egui::Stroke::new((r.width().min(r.height()) * 0.18).max(1.4), color);
    let p1 = egui::pos2(r.left(), r.top() + r.height() * 0.56);
    let p2 = egui::pos2(r.left() + r.width() * 0.38, r.bottom());
    let p3 = egui::pos2(r.right(), r.top() + r.height() * 0.14);
    painter.line_segment([p1, p2], stroke);
    painter.line_segment([p2, p3], stroke);
}

/// Draw a diagonal cross (`X`) icon inside `rect`.
pub fn close_cross(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let min_dim = rect.width().min(rect.height());
    let inner = rect.shrink(min_dim * 0.22);
    let stroke = egui::Stroke::new((min_dim * 0.14).max(1.5), color);
    painter.line_segment([inner.left_top(), inner.right_bottom()], stroke);
    painter.line_segment([inner.right_top(), inner.left_bottom()], stroke);
}

/// Draw a downward-pointing chevron triangle inside `rect`.
pub fn chevron_down(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let tri = egui::Rect::from_center_size(
        rect.center(),
        egui::vec2(rect.width() * 0.72, rect.height() * 0.48),
    );
    painter.add(egui::Shape::rotated_triangle(tri, 0.0, color));
}

/// Draw an upward-pointing chevron triangle inside `rect`.
pub fn chevron_up(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let tri = egui::Rect::from_center_size(
        rect.center(),
        egui::vec2(rect.width() * 0.72, rect.height() * 0.48),
    );
    painter.add(egui::Shape::rotated_triangle(tri, std::f32::consts::PI, color));
}

/// Draw a gear/cog icon with a genuinely transparent center hole and 8 radial teeth.
pub fn gear(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let c = rect.center();
    let r = rect.width().min(rect.height()) * 0.48;
    painter.circle_stroke(c, r * 0.56, egui::Stroke::new((r * 0.28).max(1.6), color));
    let tooth_stroke = egui::Stroke::new((r * 0.30).max(1.8), color);
    for i in 0..8 {
        let angle = i as f32 * std::f32::consts::FRAC_PI_4;
        let dir = egui::vec2(angle.cos(), angle.sin());
        painter.line_segment([c + dir * (r * 0.62), c + dir * r], tooth_stroke);
    }
}

/// Draw an upward-pointing arrow inside `rect`.
pub fn arrow_up(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let c = rect.center();
    let w = rect.width() * 0.36;
    let top = rect.top() + rect.height() * 0.14;
    let bot = rect.bottom() - rect.height() * 0.14;
    let stroke = egui::Stroke::new((rect.width().min(rect.height()) * 0.14).max(1.5), color);
    painter.line_segment([egui::pos2(c.x, bot), egui::pos2(c.x, top + 1.0)], stroke);
    painter.line_segment([egui::pos2(c.x - w, top + w), egui::pos2(c.x, top)], stroke);
    painter.line_segment([egui::pos2(c.x + w, top + w), egui::pos2(c.x, top)], stroke);
}

/// Draw a right-pointing arrow inside `rect`.
pub fn arrow_right(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let c = rect.center();
    let h = rect.height() * 0.32;
    let left = rect.left() + rect.width() * 0.12;
    let right = rect.right() - rect.width() * 0.12;
    let stroke = egui::Stroke::new((rect.width().min(rect.height()) * 0.12).max(1.5), color);
    painter.line_segment([egui::pos2(left, c.y), egui::pos2(right - 1.0, c.y)], stroke);
    painter.line_segment([egui::pos2(right - h, c.y - h), egui::pos2(right, c.y)], stroke);
    painter.line_segment([egui::pos2(right - h, c.y + h), egui::pos2(right, c.y)], stroke);
}

/// Draw a left-pointing arrow inside `rect` (e.g. Android Back navigation).
pub fn arrow_left(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let c = rect.center();
    let h = rect.height() * 0.32;
    let left = rect.left() + rect.width() * 0.14;
    let right = rect.right() - rect.width() * 0.14;
    let stroke = egui::Stroke::new((rect.width().min(rect.height()) * 0.13).max(1.5), color);
    painter.line_segment([egui::pos2(right, c.y), egui::pos2(left + 1.0, c.y)], stroke);
    painter.line_segment([egui::pos2(left + h, c.y - h), egui::pos2(left, c.y)], stroke);
    painter.line_segment([egui::pos2(left + h, c.y + h), egui::pos2(left, c.y)], stroke);
}

/// Draw a house silhouette inside `rect` (e.g. Android Home navigation).
pub fn home(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let r = rect.shrink(rect.width().min(rect.height()) * 0.10);
    let stroke = egui::Stroke::new((r.width().min(r.height()) * 0.14).max(1.4), color);
    let apex = egui::pos2(r.center().x, r.top());
    let eaves_y = r.top() + r.height() * 0.44;
    // Roof chevron
    painter.line_segment([egui::pos2(r.left(), eaves_y), apex], stroke);
    painter.line_segment([apex, egui::pos2(r.right(), eaves_y)], stroke);
    // House walls & base
    let inset = r.width() * 0.16;
    let bl = egui::pos2(r.left() + inset, r.bottom());
    let br = egui::pos2(r.right() - inset, r.bottom());
    let tl = egui::pos2(r.left() + inset, eaves_y);
    let tr = egui::pos2(r.right() - inset, eaves_y);
    painter.line_segment([tl, bl], stroke);
    painter.line_segment([tr, br], stroke);
    painter.line_segment([bl, br], stroke);
}

/// Draw a rounded square outline inside `rect` (e.g. Android Recents / Overview navigation).
pub fn recents(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let r = rect.shrink(rect.width().min(rect.height()) * 0.16);
    let stroke = egui::Stroke::new((r.width().min(r.height()) * 0.15).max(1.4), color);
    painter.rect_stroke(r, egui::CornerRadius::same(2), stroke, egui::StrokeKind::Inside);
}

/// Draw a folder silhouette inside `rect`.
pub fn folder(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let r = rect.shrink(rect.width().min(rect.height()) * 0.10);
    let tab = egui::Rect::from_min_max(
        egui::pos2(r.left(), r.top()),
        egui::pos2(r.left() + r.width() * 0.44, r.top() + r.height() * 0.24),
    );
    painter.rect_filled(tab, egui::CornerRadius { nw: 2, ne: 2, sw: 0, se: 0 }, color);
    let body = egui::Rect::from_min_max(egui::pos2(r.left(), r.top() + r.height() * 0.18), r.max);
    painter.rect_filled(body, egui::CornerRadius::same(2), color);
}

/// Draw a trash-can / delete silhouette inside `rect`.
pub fn delete(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let r = rect.shrink(rect.width().min(rect.height()) * 0.12);
    let stroke = egui::Stroke::new((r.width().min(r.height()) * 0.14).max(1.3), color);
    // Lid handle + top bar
    let lid_y = r.top() + r.height() * 0.22;
    painter.line_segment([egui::pos2(r.left(), lid_y), egui::pos2(r.right(), lid_y)], stroke);
    painter.line_segment(
        [
            egui::pos2(r.left() + r.width() * 0.32, r.top() + r.height() * 0.06),
            egui::pos2(r.right() - r.width() * 0.32, r.top() + r.height() * 0.06),
        ],
        stroke,
    );
    // Can body
    let body = egui::Rect::from_min_max(
        egui::pos2(r.left() + r.width() * 0.16, lid_y + 1.0),
        egui::pos2(r.right() - r.width() * 0.16, r.bottom()),
    );
    painter.rect_stroke(body, egui::CornerRadius::same(1), stroke, egui::StrokeKind::Inside);
    // Center vertical rib
    let cx = body.center().x;
    painter.line_segment(
        [egui::pos2(cx, body.top() + body.height() * 0.22), egui::pos2(cx, body.bottom() - body.height() * 0.18)],
        stroke,
    );
}

/// Paint-based [`egui::Atom`] for [`delete`].
pub fn delete_atom(size: f32) -> egui::Atom<'static> {
    egui::Atom::paint(egui::vec2(size, size), |ui, args| {
        delete(ui.painter(), args.rect, args.fallback_text_color);
    })
}

/// Paint-based [`egui::Atom`] for [`close_cross`].
pub fn close_cross_atom(size: f32) -> egui::Atom<'static> {
    egui::Atom::paint(egui::vec2(size, size), |ui, args| {
        close_cross(ui.painter(), args.rect, args.fallback_text_color);
    })
}

/// Paint-based [`egui::Atom`] for [`chevron_down`].
pub fn chevron_down_atom(size: f32) -> egui::Atom<'static> {
    egui::Atom::paint(egui::vec2(size, size), |ui, args| {
        chevron_down(ui.painter(), args.rect, args.fallback_text_color);
    })
}

/// Paint-based [`egui::Atom`] for [`chevron_up`].
pub fn chevron_up_atom(size: f32) -> egui::Atom<'static> {
    egui::Atom::paint(egui::vec2(size, size), |ui, args| {
        chevron_up(ui.painter(), args.rect, args.fallback_text_color);
    })
}

/// Paint-based [`egui::Atom`] for [`gear`].
pub fn gear_atom(size: f32) -> egui::Atom<'static> {
    egui::Atom::paint(egui::vec2(size, size), |ui, args| {
        gear(ui.painter(), args.rect, args.fallback_text_color);
    })
}

/// Paint-based [`egui::Atom`] for [`arrow_up`].
pub fn arrow_up_atom(size: f32) -> egui::Atom<'static> {
    egui::Atom::paint(egui::vec2(size, size), |ui, args| {
        arrow_up(ui.painter(), args.rect, args.fallback_text_color);
    })
}

/// Paint-based [`egui::Atom`] for [`folder`].
pub fn folder_atom(size: f32) -> egui::Atom<'static> {
    egui::Atom::paint(egui::vec2(size, size), |ui, args| {
        folder(ui.painter(), args.rect, args.fallback_text_color);
    })
}

/// Paint-based [`egui::Atom`] for [`arrow_right`].
pub fn arrow_right_atom(size: f32) -> egui::Atom<'static> {
    egui::Atom::paint(egui::vec2(size, size), |ui, args| {
        arrow_right(ui.painter(), args.rect, args.fallback_text_color);
    })
}

/// Paint-based [`egui::Atom`] for [`arrow_left`].
pub fn arrow_left_atom(size: f32) -> egui::Atom<'static> {
    egui::Atom::paint(egui::vec2(size, size), |ui, args| {
        arrow_left(ui.painter(), args.rect, args.fallback_text_color);
    })
}

/// Paint-based [`egui::Atom`] for [`home`].
pub fn home_atom(size: f32) -> egui::Atom<'static> {
    egui::Atom::paint(egui::vec2(size, size), |ui, args| {
        home(ui.painter(), args.rect, args.fallback_text_color);
    })
}

/// Paint-based [`egui::Atom`] for [`recents`].
pub fn recents_atom(size: f32) -> egui::Atom<'static> {
    egui::Atom::paint(egui::vec2(size, size), |ui, args| {
        recents(ui.painter(), args.rect, args.fallback_text_color);
    })
}

/// Paint-based [`egui::Atom`] for [`status_dot`] tinted with an explicit `color`.
pub fn status_dot_atom(size: f32, color: egui::Color32) -> egui::Atom<'static> {
    egui::Atom::paint(egui::vec2(size, size), move |ui, args| {
        status_dot(ui.painter(), args.rect, color);
    })
}

/// Standalone colored status dot widget.
pub fn status_dot_widget(ui: &mut egui::Ui, diameter: f32, color: egui::Color32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(diameter, diameter), egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        status_dot(ui.painter(), rect, color);
    }
    resp
}

/// Standalone colored checkmark widget (e.g. chat delivery receipt).
pub fn check_widget(ui: &mut egui::Ui, size: f32, color: egui::Color32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        check(ui.painter(), rect, color);
    }
    resp
}

/// Standalone right-arrow widget tinted with the active theme text color.
pub fn arrow_right_widget(ui: &mut egui::Ui, size: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        arrow_right(ui.painter(), rect, ui.visuals().text_color());
    }
    resp
}

/// Small clickable icon-only button with explicit minimum dimensions.
pub fn icon_small_button(ui: &mut egui::Ui, min_size: egui::Vec2, atom: egui::Atom<'_>) -> egui::Response {
    ui.add(egui::Button::new(atom).small().min_size(min_size))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{AcceptPrompt, IncomingSessionInfo, PasswordPrompt};
    use crate::chat::ChatWindow;
    use crate::connect::{ConnectScreen, TailscaleInfo, TailscalePeer};
    use crate::files::FileManagerWindow;
    use crate::power::{PowerConfirmDialog, ReconnectBannerState, ReconnectOverlay};
    use crate::session::{SessionInfo, SessionToolbar, settings_window};
    use crate::settings::Settings;
    use crate::terminal::TerminalWindow;
    use crate::tunnels::{ActiveTunnel, TunnelsWindow};
    use veloxdesk_protocol::control::{ChatMessage, DisplayInfo, FileEntry, Permissions, SystemPowerAction};

    const FORBIDDEN_MISSING_GLYPHS: [char; 8] = [
        '\u{25CF}', // black circle
        '\u{2715}', // multiplication x
        '\u{2699}', // gear
        '\u{25BE}', // black down-pointing small triangle
        '\u{25B4}', // black up-pointing small triangle
        '\u{2B06}', // upwards black arrow
        '\u{1F4C1}', // file folder
        '\u{2192}', // rightwards arrow
    ];

    fn collect_shapes(shape: &egui::epaint::Shape, out_text: &mut usize, out_non_text: &mut usize) {
        match shape {
            egui::epaint::Shape::Vec(v) => {
                for s in v {
                    collect_shapes(s, out_text, out_non_text);
                }
            }
            egui::epaint::Shape::Text(_) => *out_text += 1,
            egui::epaint::Shape::Noop => {}
            _ => *out_non_text += 1,
        }
    }

    fn verify_text_shapes_have_glyphs(ctx: &egui::Context, shape: &egui::epaint::Shape) {
        match shape {
            egui::epaint::Shape::Vec(v) => {
                for s in v {
                    verify_text_shapes_have_glyphs(ctx, s);
                }
            }
            egui::epaint::Shape::Text(ts) => {
                for section in &ts.galley.job.sections {
                    let range = section.byte_range.start.0..section.byte_range.end.0;
                    let slice = &ts.galley.job.text[range];
                    let printable: String = slice.chars().filter(|c| !c.is_control()).collect();
                    let ok = ctx.fonts_mut(|f| f.has_glyphs(&section.format.font_id, &printable));
                    assert!(ok, "missing glyph in rendered text section: {slice:?} (font {:?})", section.format.font_id);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn no_forbidden_missing_glyph_chars_in_ui_sources() {
        let src_dir = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src"));
        let mut checked_files = 0usize;
        for entry in std::fs::read_dir(src_dir).expect("read crates/ui/src") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            checked_files += 1;
            let content = std::fs::read_to_string(&path).expect("read source file");
            for (line_idx, line) in content.lines().enumerate() {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                for ch in FORBIDDEN_MISSING_GLYPHS {
                    assert!(
                        !line.contains(ch),
                        "forbidden missing-glyph char U+{:04X} found in {} line {}",
                        ch as u32,
                        path.display(),
                        line_idx + 1
                    );
                }
            }
        }
        assert!(checked_files >= 8, "expected at least 8 .rs files in crates/ui/src, got {checked_files}");
    }

    #[test]
    fn icon_primitives_and_widgets_render_headless() {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))),
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| {
            let rect = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(16.0, 16.0));
            let c = egui::Color32::WHITE;
            status_dot(ui.painter(), rect, c);
            close_cross(ui.painter(), rect, c);
            chevron_down(ui.painter(), rect, c);
            chevron_up(ui.painter(), rect, c);
            gear(ui.painter(), rect, c);
            arrow_up(ui.painter(), rect, c);
            arrow_right(ui.painter(), rect, c);
            arrow_left(ui.painter(), rect, c);
            home(ui.painter(), rect, c);
            recents(ui.painter(), rect, c);
            folder(ui.painter(), rect, c);

            status_dot_widget(ui, 10.0, egui::Color32::GREEN);
            arrow_right_widget(ui, 14.0);
            let _ = icon_small_button(ui, egui::vec2(22.0, 22.0), close_cross_atom(10.0));
            let _ = ui.small_button((chevron_down_atom(10.0), "Down"));
            let _ = icon_small_button(ui, egui::vec2(20.0, 18.0), chevron_up_atom(10.0));
            let _ = icon_small_button(ui, egui::vec2(22.0, 18.0), gear_atom(12.0));
            let _ = ui.button((arrow_up_atom(12.0), "Up"));
            let _ = ui.button((folder_atom(13.0), "Folder"));
            let _ = ui.button((arrow_right_atom(12.0), "Right"));
            let _ = ui.small_button((arrow_left_atom(12.0), "Back"));
            let _ = ui.small_button((home_atom(12.0), "Home"));
            let _ = ui.small_button((recents_atom(12.0), "Recents"));
        });
        out.textures_delta.clear();

        let mut text_count = 0usize;
        let mut non_text_count = 0usize;
        for clipped in &out.shapes {
            collect_shapes(&clipped.shape, &mut text_count, &mut non_text_count);
        }
        assert!(non_text_count >= 15, "expected vector shapes for all painted primitives and widgets, got {non_text_count}");
        assert!(text_count >= 4, "expected text shapes for atom-paired labels");
    }

    #[test]
    fn all_ui_screens_render_without_missing_glyphs() {
        let ctx = egui::Context::default();
        let make_input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0))),
            ..Default::default()
        };

        let mut settings = Settings::default();
        settings.recent.push("10.0.0.5:21118".into());
        settings.rendezvous_server = "127.0.0.1:21116".into();

        let mut connect = ConnectScreen::new(&settings);
        connect.device_id = "123456789".into();
        connect.host_running = true;
        connect.active_sessions = 1;
        connect.host_otp = "AB12CD34".into();
        connect.tailscale = TailscaleInfo {
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

        let info = SessionInfo {
            host_name: "host".into(),
            host_os: "android".into(),
            displays: vec![
                DisplayInfo { index: 0, name: "A".into(), width: 1920, height: 1080, primary: true, ..Default::default() },
                DisplayInfo { index: 1, name: "B".into(), width: 2560, height: 1440, ..Default::default() },
            ],
            active_display: 0,
            hud_visible: true,
            pointer_locked: true,
            active_gamepads: 2,
            privacy_mode: false,
            files_open: true,
            terminal_open: true,
            tunnels_open: true,
            chat_open: true,
            chat_unread: 2,
            allow_chat: true,
            allow_power: true,
            recording_active: true,
            recording_waiting_keyframe: false,
            recording_elapsed_ms: 95_000,
            recording_bytes: 12_400_000,
        };

        let mut toolbar_collapsed = SessionToolbar { expanded: false, ..Default::default() };
        let mut toolbar_expanded = SessionToolbar { expanded: true, ..Default::default() };
        let mut chat = ChatWindow::new("glyph-chat", false);
        chat.set_open(true);
        chat.push_incoming(ChatMessage {
            id: 1,
            sender_name: "Host".into(),
            text: "Hello / Привет".into(),
            ts_ms: 45_000_000,
        });
        chat.push_outgoing(2, "Me".into(), "Reply".into(), 45_060_000);
        chat.mark_acked(2);
        let power_dlg = PowerConfirmDialog::new(SystemPowerAction::Reboot, "Host".into());
        let reconnect_state = ReconnectBannerState {
            active: true,
            target: "10.0.0.5:21118".into(),
            attempt: 2,
            elapsed_secs: 10,
            max_secs: 300,
            next_retry_secs: 4,
            last_error: Some("restarting".into()),
        };
        let mut settings_open = true;
        let mut files = FileManagerWindow {
            open: true,
            entries: vec![
                FileEntry { name: "subdir".into(), is_dir: true, size: 0, modified_unix: 0 },
                FileEntry { name: "readme.md".into(), is_dir: false, size: 4096, modified_unix: 0 },
            ],
            ..Default::default()
        };
        let mut tunnels = TunnelsWindow {
            open: true,
            active: vec![ActiveTunnel {
                id: 1,
                local_addr: "127.0.0.1:8080".into(),
                remote_host: "127.0.0.1".into(),
                remote_port: 80,
            }],
            ..Default::default()
        };
        let mut pw = PasswordPrompt::new_with_methods(
            "123456789".into(),
            "0123456789abcdef0123456789abcdef".into(),
            "Permanent or one-time password".into(),
            true,
            true,
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
        let mut term = TerminalWindow { open: true, connected: true, ..Default::default() };
        term.append_output(b"VELOXDESK_OK\r\n");

        // Render all screens in both Dark and Light themes to verify zero missing glyphs or panics.
        for pref in [egui::ThemePreference::Dark, egui::ThemePreference::Light] {
            ctx.set_theme(pref);
            // Pass 1: ConnectScreen (fullscreen = true) + collapsed toolbar (2 passes so anchored Areas complete sizing)
            for _ in 0..2 {
                let mut out1 = ctx.run_ui(make_input(), |ui| {
                    let _ = connect.show(ui, &mut settings, true);
                    let _ = toolbar_collapsed.show(ui, &info, &mut settings);
                });
                out1.textures_delta.clear();
                for clipped in &out1.shapes {
                    verify_text_shapes_have_glyphs(&ctx, &clipped.shape);
                }
            }

            // Pass 2: Expanded toolbar + all modal/auxiliary windows
            let mut out2 = ctx.run_ui(make_input(), |ui| {
                let _ = toolbar_expanded.show(ui, &info, &mut settings);
                let _ = settings_window(ui.ctx(), &mut settings_open, &mut settings);
                let _ = files.show(ui.ctx());
                let _ = tunnels.show(ui.ctx());
                let _ = pw.show(ui.ctx());
                let _ = accept.show(ui.ctx());
                let _ = term.show(ui.ctx());
                let _ = chat.show(ui.ctx());
                let _ = power_dlg.show(ui.ctx());
                let _ = ReconnectOverlay::show(ui.ctx(), &reconnect_state);
            });
            out2.textures_delta.clear();
            for clipped in &out2.shapes {
                verify_text_shapes_have_glyphs(&ctx, &clipped.shape);
            }
        }

        // Verify default_fonts retains full Cyrillic + punctuation coverage (for R3)
        let cyrillic_and_punct =
            "АБВГДЕЁЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯабвгдеёжзийклмнопрстуфхцчшщъыьэюя№«»•—…·×";
        assert!(
            ctx.fonts_mut(|f| f.has_glyphs(&egui::FontId::proportional(14.0), cyrillic_and_punct)),
            "default proportional font must retain full Cyrillic and UI punctuation glyphs"
        );
        assert!(
            ctx.fonts_mut(|f| f.has_glyphs(&egui::FontId::monospace(12.5), cyrillic_and_punct)),
            "default monospace font must retain full Cyrillic and UI punctuation glyphs"
        );
    }
}
