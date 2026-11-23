//! v0.2.0 Release Integration Tests (R1–R7).
//!
//! Covers:
//! - R1: Fullscreen exit failsafe & session disconnect chrome transitions.
//! - R2: Automated source scan for forbidden missing-glyph Unicode icons across
//!   `crates/ui/src` and `crates/render/src`, plus headless `egui` smoke renders
//!   with font-glyph verification in both EN and RU.
//! - R3: Complete EN/RU i18n catalog (`Strings::all_fields()`) and `Language` serde round-trip.
//! - R4: Completely independent `Settings.desktop` vs `Settings.gaming` streaming profiles,
//!   JSON save/load persistence, and legacy flat JSON migration.
//! - R5: Complete EN/RU Settings hover tooltips (`tip_*`) and live hover rendering across
//!   both languages and both Desktop/Gaming profiles.
//! - R6 & R7: Version + Git SHA display format (`v0.2.0 (<sha>)`), `veloxdesk --version` CLI
//!   verification (when compiled), and update banner/Settings update actions.

use std::path::{Path, PathBuf};
use std::process::Command;

use veloxdesk_protocol::control::{DisplayInfo, FileEntry, Permissions};
use veloxdesk_ui::{
    AcceptPrompt, ActiveTunnel, ConnectAction, ConnectScreen, FileManagerWindow, FpsCap, IncomingSessionInfo, Language,
    Mode, PasswordPrompt, Present, Quality, Scale, SessionInfo, SessionToolbar, Settings, StreamSettings,
    TailscaleInfo, TailscalePeer, TerminalWindow, ToolbarAction, TunnelsWindow, UpdateUiState, settings_window_full,
    tr,
};

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is `<workspace>/tests`.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tests crate must live under workspace root")
        .to_path_buf()
}

fn make_input(viewport: egui::Vec2, time: Option<f64>, events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, viewport)),
        time,
        events,
        ..Default::default()
    }
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

fn shapes_contain_text(shapes: &[egui::epaint::ClippedShape], needle: &str) -> bool {
    shapes.iter().any(|c| find_text_rect(&c.shape, needle).is_some())
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
                assert!(
                    ok,
                    "rendered text contains missing glyph in egui bundled font: {slice:?} (font {:?})",
                    section.format.font_id
                );
            }
        }
        _ => {}
    }
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("failed to read {}: {e}", dir.display()));
    for entry in entries {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// Strip `#[cfg(test)]` tail module (if present) and single-line `//` comments so the R2 scan
/// checks only production UI/render code (string literals and identifiers), not unit-test
/// glyph blacklists or explanatory doc comments.
fn strip_tests_and_comments(src: &str) -> String {
    let prod = src.split("\n#[cfg(test)]").next().unwrap_or(src);
    let mut out = String::with_capacity(prod.len());
    for line in prod.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            out.push('\n');
            continue;
        }
        // Keep code portion up to an inline `//` comment outside string literals.
        let mut in_str = false;
        let mut escaped = false;
        let mut chars = line.char_indices().peekable();
        let mut cut = line.len();
        while let Some((idx, ch)) = chars.next() {
            if in_str {
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    in_str = false;
                }
            } else if ch == '"' {
                in_str = true;
            } else if ch == '/' && chars.peek().is_some_and(|(_, next)| *next == '/') {
                cut = idx;
                break;
            }
        }
        out.push_str(&line[..cut]);
        out.push('\n');
    }
    out
}

fn contains_cyrillic(s: &str) -> bool {
    s.chars().any(|c| ('\u{0400}'..='\u{04FF}').contains(&c))
}

fn is_valid_build_display(display: &str, expected_ver: &str) -> bool {
    let prefix = format!("v{expected_ver} (");
    let Some(rest) = display.strip_prefix(&prefix) else {
        return false;
    };
    let Some(sha) = rest.strip_suffix(')') else {
        return false;
    };
    sha == "dev" || (sha.len() == 7 && sha.bytes().all(|b| b.is_ascii_hexdigit()))
}

// ---------------------------------------------------------------------------
// R1: Fullscreen exit failsafe & session disconnect UI/state transitions
// ---------------------------------------------------------------------------

#[test]
fn r1_fullscreen_exit_failsafe_and_toolbar_disconnect() {
    let viewport = egui::vec2(1280.0, 720.0);

    for lang in Language::ALL {
        let ctx = egui::Context::default();
        let t = tr(lang);
        let mut settings = Settings { language: lang, ..Default::default() };
        let mut screen = ConnectScreen::new(&settings);

        // When windowed, the failsafe Exit Fullscreen button must NOT render.
        let mut out_win = ctx.run_ui(make_input(viewport, Some(0.0), Vec::new()), |ui| {
            assert!(screen.show(ui, &mut settings, false).is_none());
        });
        out_win.textures_delta.clear();
        assert!(
            !shapes_contain_text(&out_win.shapes, t.exit_fullscreen_btn),
            "Exit Fullscreen button must stay hidden when is_fullscreen=false ({lang:?})"
        );

        // When fullscreen, the failsafe button renders and clicking it emits ExitFullscreen.
        let mut fs_pos = None;
        for step in 0..3 {
            let mut out = ctx.run_ui(make_input(viewport, Some(0.02 * step as f64), Vec::new()), |ui| {
                assert!(screen.show(ui, &mut settings, true).is_none());
            });
            out.textures_delta.clear();
            fs_pos = out
                .shapes
                .iter()
                .find_map(|c| find_text_rect(&c.shape, t.exit_fullscreen_btn))
                .map(|r| r.center());
        }
        let btn_pos = fs_pos.unwrap_or_else(|| panic!("Exit Fullscreen button missing for {lang:?}"));

        let mut emitted = None;
        for (i, pressed) in [true, false].into_iter().enumerate() {
            let mut out = ctx.run_ui(
                make_input(
                    viewport,
                    Some(0.10 + 0.02 * i as f64),
                    vec![
                        egui::Event::PointerMoved(btn_pos),
                        egui::Event::PointerButton {
                            pos: btn_pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                ),
                |ui| {
                    emitted = screen.show(ui, &mut settings, true);
                },
            );
            out.textures_delta.clear();
        }
        assert_eq!(
            emitted,
            Some(ConnectAction::ExitFullscreen),
            "clicking failsafe button must emit ConnectAction::ExitFullscreen ({lang:?})"
        );

        // SessionToolbar expanded: clicking Fullscreen emits ToggleFullscreen, clicking Disconnect emits Disconnect.
        let tb_ctx = egui::Context::default();
        let info = SessionInfo {
            host_name: "desk-01".into(),
            displays: vec![DisplayInfo {
                index: 0,
                name: "Primary".into(),
                width: 1920,
                height: 1080,
                primary: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut tb = SessionToolbar { expanded: true };
        let mut warm = tb_ctx.run_ui(make_input(viewport, Some(0.0), Vec::new()), |ui| {
            let _ = tb.show(ui, &info, &mut settings);
        });
        warm.textures_delta.clear();
        warm = tb_ctx.run_ui(make_input(viewport, Some(0.02), Vec::new()), |ui| {
            let _ = tb.show(ui, &info, &mut settings);
        });
        warm.textures_delta.clear();

        for (label, expected_action) in [
            (t.toolbar_fullscreen, ToolbarAction::ToggleFullscreen),
            (t.toolbar_disconnect, ToolbarAction::Disconnect),
        ] {
            let pos = warm
                .shapes
                .iter()
                .find_map(|c| find_text_rect(&c.shape, label))
                .unwrap_or_else(|| panic!("toolbar button {label:?} not rendered ({lang:?})"))
                .center();
            let mut actions = Vec::new();
            for (i, pressed) in [true, false].into_iter().enumerate() {
                let mut out = tb_ctx.run_ui(
                    make_input(
                        viewport,
                        Some(0.10 + 0.02 * i as f64),
                        vec![
                            egui::Event::PointerMoved(pos),
                            egui::Event::PointerButton {
                                pos,
                                button: egui::PointerButton::Primary,
                                pressed,
                                modifiers: egui::Modifiers::NONE,
                            },
                        ],
                    ),
                    |ui| {
                        actions = tb.show(ui, &info, &mut settings);
                    },
                );
                out.textures_delta.clear();
            }
            assert!(
                actions.contains(&expected_action),
                "clicking toolbar {label:?} must emit {expected_action:?}, got {actions:?} ({lang:?})"
            );
        }
    }

    // Verify client app state machine resets fullscreen & decorations on session end.
    let app_rs = std::fs::read_to_string(workspace_root().join("apps/client/src/app.rs"))
        .expect("apps/client/src/app.rs must exist");
    assert!(app_rs.contains("fn end_session"), "WindowChromeState::end_session missing in app.rs");
    assert!(app_rs.contains("fn exit_fullscreen"), "WindowChromeState::exit_fullscreen missing in app.rs");
    assert!(app_rs.contains("fn apply_window_chrome"), "apply_window_chrome missing in app.rs");
    assert!(app_rs.contains("set_fullscreen("), "app.rs must call set_fullscreen(...)");
    assert!(app_rs.contains("set_decorations(true)"), "app.rs must restore set_decorations(true)");
}

// ---------------------------------------------------------------------------
// R2: Zero missing-glyph Unicode icons + headless EN/RU smoke renders
// ---------------------------------------------------------------------------

#[test]
fn r2_no_forbidden_unicode_icons_in_ui_and_render_sources() {
    // Forbidden glyphs that render as empty boxes (tofu) in egui's default bundled fonts.
    const FORBIDDEN_ICONS: &[(char, &str)] = &[
        ('\u{25CF}', "BLACK CIRCLE ●"),
        ('\u{25CB}', "WHITE CIRCLE ○"),
        ('\u{2715}', "MULTIPLICATION X ✕"),
        ('\u{2716}', "HEAVY MULTIPLICATION X ✖"),
        ('\u{2699}', "GEAR ⚙"),
        ('\u{1F512}', "LOCK 🔒"),
        ('\u{1F513}', "UNLOCK 🔓"),
        ('\u{1F4CB}', "CLIPBOARD 📋"),
        ('\u{1F4C1}', "FILE FOLDER 📁"),
        ('\u{1F5A5}', "DESKTOP COMPUTER 🖥"),
        ('\u{1F3AE}', "VIDEO GAME 🎮"),
        ('\u{1F5D1}', "WASTEBASKET 🗑"),
        ('\u{26A0}', "WARNING SIGN ⚠"),
        ('\u{2713}', "CHECK MARK ✓"),
        ('\u{2714}', "HEAVY CHECK MARK ✔"),
        ('\u{2717}', "BALLOT X ✗"),
        ('\u{1F504}', "ANTICLOCKWISE ARROWS 🔄"),
        ('\u{26A1}', "HIGH VOLTAGE ⚡"),
        ('\u{2139}', "INFORMATION SOURCE ℹ"),
        ('\u{1F50A}', "SPEAKER HIGH VOLUME 🔊"),
        ('\u{1F507}', "SPEAKER CANCELLED 🔇"),
        ('\u{1F3A4}', "MICROPHONE 🎤"),
        ('\u{25B6}', "PLAY ▶"),
        ('\u{23F8}', "PAUSE ⏸"),
        ('\u{23F9}', "STOP ⏹"),
        ('\u{26F6}', "SQUARE FOUR CORNERS ⛶"),
        ('\u{2605}', "BLACK STAR ★"),
        ('\u{2606}', "WHITE STAR ☆"),
        ('\u{25C0}', "BLACK LEFT TRIANGLE ◀"),
        ('\u{25B2}', "BLACK UP TRIANGLE ▲"),
        ('\u{25BC}', "BLACK DOWN TRIANGLE ▼"),
        ('\u{25BE}', "SMALL DOWN TRIANGLE ▾"),
        ('\u{25B4}', "SMALL UP TRIANGLE ▴"),
        ('\u{2B06}', "UPWARDS BLACK ARROW ⬆"),
        ('\u{2192}', "RIGHTWARDS ARROW →"),
    ];

    let root = workspace_root();
    let mut files = Vec::new();
    collect_rs_files(&root.join("crates/ui/src"), &mut files);
    collect_rs_files(&root.join("crates/render/src"), &mut files);
    assert!(!files.is_empty(), "expected .rs files under crates/ui/src and crates/render/src");

    let mut violations = Vec::new();
    for path in files {
        let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let code = strip_tests_and_comments(&raw);
        for (line_no, line) in code.lines().enumerate() {
            for &(ch, desc) in FORBIDDEN_ICONS {
                if line.contains(ch) {
                    violations.push(format!(
                        "{}:{} contains forbidden missing-glyph char U+{:04X} ({desc}): {}",
                        path.strip_prefix(&root).unwrap_or(&path).display(),
                        line_no + 1,
                        ch as u32,
                        line.trim()
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Found forbidden missing-glyph Unicode icons in UI/render sources:\n{}",
        violations.join("\n")
    );
}

#[test]
fn r2_headless_render_all_ui_components_en_and_ru_with_glyph_check() {
    let viewport = egui::vec2(1280.0, 720.0);

    for lang in Language::ALL {
        let ctx = egui::Context::default();
        let t = tr(lang);
        let mut settings = Settings {
            language: lang,
            rendezvous_server: "rdv.example.com:21116".into(),
            ..Default::default()
        };
        settings.recent.push("192.168.1.42:21118".into());

        let mut screen = ConnectScreen::new(&settings);
        screen.device_id = "123456789".into();
        screen.host_running = true;
        screen.active_sessions = 1;
        screen.host_status = t.host_listening.into();
        screen.host_otp = "ABCD1234".into();
        screen.build_display = "v0.2.0 (26e488f)".into();
        screen.tailscale = TailscaleInfo {
            installed: true,
            running: true,
            self_ip: Some("100.64.0.10".into()),
            self_name: Some("workstation".into()),
            peers: vec![
                TailscalePeer {
                    name: "gaming-rig".into(),
                    ip: "100.64.0.20".into(),
                    os: "windows".into(),
                    online: true,
                },
                TailscalePeer {
                    name: "laptop".into(),
                    ip: "100.64.0.30".into(),
                    os: "linux".into(),
                    online: false,
                },
            ],
            error: None,
        };

        // Render ConnectScreen across every UpdateUiState variant (R7) + fullscreen failsafe (R1).
        let update_states = [
            UpdateUiState::Idle,
            UpdateUiState::Checking,
            UpdateUiState::UpToDate,
            UpdateUiState::Available {
                current: "0.2.0".into(),
                latest: "0.3.0".into(),
                notes: "Release notes summary".into(),
                html_url: "https://github.com/Zerrant2/VeloxDesk/releases/tag/v0.3.0".into(),
            },
            UpdateUiState::Downloading {
                progress: Some(0.65),
                status: t.fmt_update_downloading_bytes(650_000, Some(1_000_000)),
            },
            UpdateUiState::Error("network unreachable".into()),
        ];

        for (idx, state) in update_states.iter().enumerate() {
            screen.update_state = state.clone();
            let is_fs = idx % 2 == 1;
            let mut out = ctx.run_ui(make_input(viewport, Some(0.05 * idx as f64), Vec::new()), |ui| {
                let _ = screen.show(ui, &mut settings, is_fs);
            });
            out.textures_delta.clear();
            for clipped in &out.shapes {
                verify_text_shapes_have_glyphs(&ctx, &clipped.shape);
            }
        }

        // Render SessionToolbar (collapsed & expanded), settings_window_full, AcceptPrompt,
        // PasswordPrompt, FileManagerWindow, TerminalWindow, and TunnelsWindow.
        let info = SessionInfo {
            host_name: "remote-host".into(),
            displays: vec![
                DisplayInfo { index: 0, name: "Main".into(), width: 2560, height: 1440, primary: true, ..Default::default() },
                DisplayInfo { index: 1, name: "Side".into(), width: 1920, height: 1080, ..Default::default() },
            ],
            active_display: 0,
            hud_visible: true,
            pointer_locked: true,
            active_gamepads: 2,
            privacy_mode: false,
            files_open: true,
            terminal_open: true,
            tunnels_open: true,
        };
        let mut tb_collapsed = SessionToolbar { expanded: false };
        let mut tb_expanded = SessionToolbar { expanded: true };
        let mut settings_open = true;
        let mut pw = PasswordPrompt::new(
            "123456789".into(),
            "0123456789abcdef0123456789abcdef".into(),
            t.auth_methods_both.into(),
        );
        let mut accept = AcceptPrompt::new(
            IncomingSessionInfo {
                peer_device_id: "987654321".into(),
                peer_name: "Gaming-PC".into(),
                peer_fingerprint: "abcdef0123456789abcdef0123456789".into(),
                peer_public_key: [9u8; 32],
            },
            Permissions { view: true, input: true, clipboard: true, ..Default::default() },
        );
        let mut files = FileManagerWindow {
            open: true,
            entries: vec![
                FileEntry { name: "docs".into(), is_dir: true, size: 0, modified_unix: 0 },
                FileEntry { name: "report.pdf".into(), is_dir: false, size: 12_345, modified_unix: 0 },
            ],
            ..Default::default()
        };
        let mut term = TerminalWindow { open: true, connected: true, ..Default::default() };
        term.append_output(b"VeloxDesk OK\r\n");
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

        let update_avail = UpdateUiState::Available {
            current: "0.2.0".into(),
            latest: "0.2.1".into(),
            notes: "Patch release".into(),
            html_url: "https://github.com/Zerrant2/VeloxDesk/releases/tag/v0.2.1".into(),
        };

        for mode in [Mode::Desktop, Mode::Gaming] {
            settings.mode = mode;

            // Render collapsed toolbar in its own pass so it does not share widget IDs with expanded toolbar.
            let mut out_collapsed = ctx.run_ui(make_input(viewport, Some(0.95), Vec::new()), |ui| {
                let _ = tb_collapsed.show(ui, &info, &mut settings);
            });
            out_collapsed.textures_delta.clear();
            for clipped in &out_collapsed.shapes {
                verify_text_shapes_have_glyphs(&ctx, &clipped.shape);
            }

            // Run two frames so egui::Window completes its initial auto-sizing pass and paints all text.
            let mut last_shapes = Vec::new();
            for step in 0..2 {
                let mut out = ctx.run_ui(make_input(viewport, Some(1.0 + 0.02 * step as f64), Vec::new()), |ui| {
                    let _ = tb_expanded.show(ui, &info, &mut settings);
                    let _ = settings_window_full(
                        ui.ctx(),
                        &mut settings_open,
                        &mut settings,
                        None,
                        Some("v0.2.0 (26e488f)"),
                        Some(&update_avail),
                    );
                    let _ = pw.show_for(ui.ctx(), lang);
                    let _ = accept.show_for(ui.ctx(), lang);
                    let _ = files.show_for(ui.ctx(), lang);
                    let _ = term.show_for(ui.ctx(), lang);
                    let _ = tunnels.show_for(ui.ctx(), lang);
                });
                out.textures_delta.clear();
                for clipped in &out.shapes {
                    verify_text_shapes_have_glyphs(&ctx, &clipped.shape);
                }
                last_shapes = out.shapes;
            }
            assert!(
                shapes_contain_text(&last_shapes, t.settings_title),
                "settings window title missing for {lang:?} / {mode:?}"
            );
            assert!(
                shapes_contain_text(&last_shapes, t.incoming_connection_title),
                "AcceptPrompt title missing for {lang:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// R3: EN / RU i18n completeness & Language serde round-trip
// ---------------------------------------------------------------------------

#[test]
fn r3_i18n_en_and_ru_completeness_and_language_serde() {
    let ctx = egui::Context::default();
    let mut init = ctx.run_ui(egui::RawInput::default(), |_| {});
    init.textures_delta.clear();

    let en_fields = tr(Language::En).all_fields();
    let ru_fields = tr(Language::Ru).all_fields();

    assert!(en_fields.len() >= 150, "expected full i18n catalog, got {} fields", en_fields.len());
    assert_eq!(en_fields.len(), ru_fields.len(), "EN and RU field counts must match");

    let mut ru_cyrillic_count = 0usize;
    for ((en_name, en_val), (ru_name, ru_val)) in en_fields.iter().zip(ru_fields.iter()) {
        assert_eq!(en_name, ru_name, "field order mismatch between EN and RU");
        assert!(!en_val.trim().is_empty(), "empty EN string for {en_name}");
        assert!(!ru_val.trim().is_empty(), "empty RU string for {ru_name}");
        if contains_cyrillic(ru_val) {
            ru_cyrillic_count += 1;
        }
        assert!(
            ctx.fonts_mut(|f| f.has_glyphs(&egui::FontId::proportional(14.0), en_val)),
            "missing proportional glyph in EN.{en_name}: {en_val:?}"
        );
        assert!(
            ctx.fonts_mut(|f| f.has_glyphs(&egui::FontId::proportional(14.0), ru_val)),
            "missing proportional glyph in RU.{ru_name}: {ru_val:?}"
        );
        assert!(
            ctx.fonts_mut(|f| f.has_glyphs(&egui::FontId::monospace(12.5), ru_val)),
            "missing monospace glyph in RU.{ru_name}: {ru_val:?}"
        );
    }

    let cyrillic_ratio = ru_cyrillic_count as f64 / ru_fields.len() as f64;
    assert!(
        cyrillic_ratio >= 0.85,
        "expected >= 85% of RU strings to contain Cyrillic (remaining are brand/FPS/HUD/VSync terms), got {:.1}% ({ru_cyrillic_count}/{})",
        cyrillic_ratio * 100.0,
        ru_fields.len()
    );

    // Language serde + u8 round-trip + autonym labels.
    for lang in Language::ALL {
        let json = serde_json::to_string(&lang).expect("serialize Language");
        let parsed: Language = serde_json::from_str(&json).expect("deserialize Language");
        assert_eq!(parsed, lang);
        assert_eq!(Language::from_u8(lang.to_u8()), lang);
        assert!(!lang.label().is_empty());
        assert!(!lang.short_label().is_empty());
    }
    assert_eq!(Language::En.short_label(), "EN");
    assert_eq!(Language::Ru.short_label(), "RU");
    assert!(contains_cyrillic(Language::Ru.label()));
}

// ---------------------------------------------------------------------------
// R4: Independent Desktop & Gaming profiles + persistence + legacy migration
// ---------------------------------------------------------------------------

#[test]
fn r4_desktop_and_gaming_profiles_independent_and_persist() {
    let desktop_cfg = StreamSettings {
        quality: Quality::Low,
        fps_cap: FpsCap::Fps60,
        present: Present::Fifo,
        scale: Scale::Original,
        show_hud: false,
        prefer_hw_decode: false,
        relative_mouse_in_gaming: false,
        hdr_10bit: false,
        yuv444: true,
        audio_enabled: false,
        mic_enabled: false,
        clipboard_sync: true,
    };
    let gaming_cfg = StreamSettings {
        quality: Quality::Ultra,
        fps_cap: FpsCap::Fps240,
        present: Present::Immediate,
        scale: Scale::Stretch,
        show_hud: true,
        prefer_hw_decode: true,
        relative_mouse_in_gaming: true,
        hdr_10bit: true,
        yuv444: false,
        audio_enabled: true,
        mic_enabled: true,
        clipboard_sync: false,
    };

    // Every single field differs between desktop_cfg and gaming_cfg.
    assert_ne!(desktop_cfg.quality, gaming_cfg.quality);
    assert_ne!(desktop_cfg.fps_cap, gaming_cfg.fps_cap);
    assert_ne!(desktop_cfg.present, gaming_cfg.present);
    assert_ne!(desktop_cfg.scale, gaming_cfg.scale);
    assert_ne!(desktop_cfg.show_hud, gaming_cfg.show_hud);
    assert_ne!(desktop_cfg.prefer_hw_decode, gaming_cfg.prefer_hw_decode);
    assert_ne!(desktop_cfg.relative_mouse_in_gaming, gaming_cfg.relative_mouse_in_gaming);
    assert_ne!(desktop_cfg.hdr_10bit, gaming_cfg.hdr_10bit);
    assert_ne!(desktop_cfg.yuv444, gaming_cfg.yuv444);
    assert_ne!(desktop_cfg.audio_enabled, gaming_cfg.audio_enabled);
    assert_ne!(desktop_cfg.mic_enabled, gaming_cfg.mic_enabled);
    assert_ne!(desktop_cfg.clipboard_sync, gaming_cfg.clipboard_sync);

    let mut settings = Settings {
        language: Language::Ru,
        recent: vec!["10.0.0.5:21118".into()],
        rendezvous_server: "rdv.example.com:21116".into(),
        mode: Mode::Desktop,
        desktop: desktop_cfg,
        gaming: gaming_cfg,
        require_accept: false,
    };

    // active() / active_mut() tracks mode without leaking across profiles.
    assert_eq!(*settings.active(), desktop_cfg);
    settings.active_mut().quality = Quality::High;
    settings.active_mut().fps_cap = FpsCap::Fps144;
    settings.active_mut().show_hud = true;
    assert_eq!(settings.gaming, gaming_cfg, "mutating Desktop profile must never touch Gaming");

    let desktop_modified = settings.desktop;
    settings.mode = Mode::Gaming;
    assert_eq!(*settings.active(), gaming_cfg);
    settings.active_mut().hdr_10bit = false;
    settings.active_mut().mic_enabled = false;
    assert_eq!(settings.desktop, desktop_modified, "mutating Gaming profile must never touch Desktop");

    // Disk JSON save/load round-trip preserves both independent profiles and Language.
    let dir = std::env::temp_dir().join(format!("veloxdesk-r4-test-{}", std::process::id()));
    let path = dir.join("settings.json");
    settings.save(&path).expect("save settings JSON");
    let loaded = Settings::load(&path).expect("load settings JSON");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(loaded, settings);
    assert_eq!(loaded.desktop, desktop_modified);
    assert_eq!(loaded.gaming, settings.gaming);

    // Legacy flat JSON (pre-R4, no nested `desktop`/`gaming` objects) migrates safely.
    let legacy_json = r#"{
        "language": "Ru",
        "mode": "Gaming",
        "quality": "Ultra",
        "fps_cap": "Fps240",
        "present": "Immediate",
        "scale": "Stretch",
        "show_hud": true,
        "prefer_hw_decode": false,
        "relative_mouse_in_gaming": false,
        "hdr_10bit": true,
        "yuv444": true,
        "audio_enabled": false,
        "mic_enabled": true,
        "clipboard_sync": false,
        "rendezvous_server": "127.0.0.1:21116"
    }"#;
    let migrated: Settings = serde_json::from_str(legacy_json).expect("deserialize legacy flat JSON");
    assert_eq!(migrated.language, Language::Ru);
    assert_eq!(migrated.mode, Mode::Gaming);
    assert_eq!(migrated.gaming.quality, Quality::Ultra);
    assert_eq!(migrated.gaming.fps_cap, FpsCap::Fps240);
    assert_eq!(migrated.gaming.present, Present::Immediate);
    assert_eq!(migrated.gaming.scale, Scale::Stretch);
    assert!(migrated.gaming.show_hud);
    assert!(!migrated.gaming.prefer_hw_decode);
    assert!(!migrated.gaming.relative_mouse_in_gaming);
    assert!(migrated.gaming.hdr_10bit);
    assert!(migrated.gaming.yuv444);
    assert!(!migrated.gaming.audio_enabled);
    assert!(migrated.gaming.mic_enabled);
    assert!(!migrated.gaming.clipboard_sync);
    // Desktop profile remains clean defaults (uncorrupted by Gaming legacy fields).
    assert_eq!(migrated.desktop, StreamSettings::default_for(Mode::Desktop));
}

// ---------------------------------------------------------------------------
// R5: Settings hover tooltips (EN & RU) + rendering in Desktop & Gaming tabs
// ---------------------------------------------------------------------------

#[test]
fn r5_settings_tooltips_complete_and_render_in_both_languages_and_profiles() {
    let en_tips: Vec<(&str, &str)> = tr(Language::En)
        .all_fields()
        .into_iter()
        .filter(|(k, _)| k.starts_with("tip_"))
        .collect();
    let ru_tips: Vec<(&str, &str)> = tr(Language::Ru)
        .all_fields()
        .into_iter()
        .filter(|(k, _)| k.starts_with("tip_"))
        .collect();

    assert!(
        en_tips.len() >= 18,
        "expected >= 18 settings tip_* tooltips, found {}",
        en_tips.len()
    );
    assert_eq!(en_tips.len(), ru_tips.len());

    for ((ek, ev), (rk, rv)) in en_tips.iter().zip(ru_tips.iter()) {
        assert_eq!(ek, rk);
        assert!(!ev.trim().is_empty(), "EN tooltip {ek} must not be empty");
        assert!(!rv.trim().is_empty(), "RU tooltip {rk} must not be empty");
        assert_ne!(ev, rv, "tooltip {ek} must be translated between EN and RU");
        assert!(contains_cyrillic(rv), "RU tooltip {rk} must contain Cyrillic: {rv:?}");
    }

    // Verify hover tooltip popup renders cleanly in EN & RU across Desktop and Gaming modes.
    let viewport = egui::vec2(1280.0, 720.0);
    for lang in Language::ALL {
        let t = tr(lang);
        for mode in [Mode::Desktop, Mode::Gaming] {
            let ctx = egui::Context::default();
            let mut settings = Settings { language: lang, mode, ..Default::default() };
            let mut open = true;

            let mut init_frame = ctx.run_ui(make_input(viewport, Some(0.0), Vec::new()), |ui| {
                let _ = settings_window_full(
                    ui.ctx(),
                    &mut open,
                    &mut settings,
                    None,
                    Some("v0.2.0 (26e488f)"),
                    Some(&UpdateUiState::Idle),
                );
            });
            init_frame.textures_delta.clear();
            let mut warm = ctx.run_ui(make_input(viewport, Some(0.02), Vec::new()), |ui| {
                let _ = settings_window_full(
                    ui.ctx(),
                    &mut open,
                    &mut settings,
                    None,
                    Some("v0.2.0 (26e488f)"),
                    Some(&UpdateUiState::Idle),
                );
            });
            warm.textures_delta.clear();

            let quality_pos = warm
                .shapes
                .iter()
                .find_map(|c| find_text_rect(&c.shape, t.settings_quality))
                .unwrap_or_else(|| panic!("Quality label missing for {lang:?} / {mode:?}"))
                .center();

            let mut moved = ctx.run_ui(
                make_input(viewport, Some(0.10), vec![egui::Event::PointerMoved(quality_pos)]),
                |ui| {
                    let _ = settings_window_full(
                        ui.ctx(),
                        &mut open,
                        &mut settings,
                        None,
                        Some("v0.2.0 (26e488f)"),
                        Some(&UpdateUiState::Idle),
                    );
                },
            );
            moved.textures_delta.clear();

            for time in [0.75, 0.77] {
                let mut out = ctx.run_ui(make_input(viewport, Some(time), Vec::new()), |ui| {
                    let _ = settings_window_full(
                        ui.ctx(),
                        &mut open,
                        &mut settings,
                        None,
                        Some("v0.2.0 (26e488f)"),
                        Some(&UpdateUiState::Idle),
                    );
                });
                out.textures_delta.clear();
                if time > 0.76 {
                    assert!(
                        shapes_contain_text(&out.shapes, t.tip_quality),
                        "expected hover tooltip {:?} after delay ({lang:?}, {mode:?})",
                        t.tip_quality
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// R6 & R7: Version + commit display format, CLI --version, and update UI
// ---------------------------------------------------------------------------

#[test]
fn r6_r7_version_display_build_script_and_update_actions() {
    let root = workspace_root();

    // 1. Workspace Cargo.toml version and build.rs format contract.
    let root_toml = std::fs::read_to_string(root.join("Cargo.toml")).expect("read root Cargo.toml");
    assert!(
        root_toml.contains("version = \"0.2.0\""),
        "workspace version in root Cargo.toml must be 0.2.0"
    );

    let build_rs = std::fs::read_to_string(root.join("apps/client/build.rs")).expect("read apps/client/build.rs");
    assert!(build_rs.contains("VELOXDESK_GIT_SHA"), "build.rs must emit VELOXDESK_GIT_SHA");
    assert!(
        build_rs.contains("VELOXDESK_BUILD_DISPLAY"),
        "build.rs must emit VELOXDESK_BUILD_DISPLAY"
    );
    assert!(
        build_rs.contains("v{pkg_ver} ({sha})"),
        "build.rs must format VELOXDESK_BUILD_DISPLAY as v<semver> (<sha>)"
    );

    assert!(is_valid_build_display("v0.2.0 (26e488f)", "0.2.0"));
    assert!(is_valid_build_display("v0.2.0 (dev)", "0.2.0"));
    assert!(!is_valid_build_display("0.2.0 (26e488f)", "0.2.0"));
    assert!(!is_valid_build_display("v0.2.0 (12345)", "0.2.0"));

    // 2. If veloxdesk binary is compiled in the same target profile dir, verify `--version` output live.
    if let Ok(exe) = std::env::current_exe() {
        let bin_name = if cfg!(windows) { "veloxdesk.exe" } else { "veloxdesk" };
        let client_bin = exe
            .ancestors()
            .skip(1)
            .map(|dir| dir.join(bin_name))
            .find(|cand| cand.is_file());
        if let Some(client_bin) = client_bin {
            let output = Command::new(&client_bin)
                .arg("--version")
                .output()
                .unwrap_or_else(|e| panic!("failed to run {} --version: {e}", client_bin.display()));
            assert!(
                output.status.success(),
                "veloxdesk --version failed: stderr={}",
                String::from_utf8_lossy(&output.stderr)
            );
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let display = stdout
                .strip_prefix("veloxdesk ")
                .unwrap_or_else(|| panic!("expected `veloxdesk v0.2.0 (<sha>)`, got {stdout:?}"));
            assert!(
                is_valid_build_display(display, "0.2.0"),
                "unexpected `veloxdesk --version` format: {stdout:?}"
            );
        }
    }

    // 3. Verify ConnectScreen R7 update banner renders and emits StartUpdate & DismissUpdate.
    let viewport = egui::vec2(1280.0, 720.0);
    for lang in Language::ALL {
        let t = tr(lang);
        let ctx = egui::Context::default();
        let mut settings = Settings { language: lang, ..Default::default() };
        let mut screen = ConnectScreen::new(&settings);
        screen.build_display = "v0.2.0 (26e488f)".into();
        screen.update_state = UpdateUiState::Available {
            current: "0.2.0".into(),
            latest: "0.3.0".into(),
            notes: "Faster encoding".into(),
            html_url: "https://github.com/Zerrant2/VeloxDesk/releases/tag/v0.3.0".into(),
        };

        let mut warm = ctx.run_ui(make_input(viewport, Some(0.0), Vec::new()), |ui| {
            let _ = screen.show(ui, &mut settings, false);
        });
        warm.textures_delta.clear();
        assert!(
            shapes_contain_text(&warm.shapes, "v0.2.0 (26e488f)"),
            "ConnectScreen must display build_display ({lang:?})"
        );

        let update_btn = warm
            .shapes
            .iter()
            .find_map(|c| find_text_rect(&c.shape, t.update_now_btn))
            .unwrap_or_else(|| panic!("Update & Restart button missing ({lang:?})"))
            .center();
        let mut action = None;
        for (i, pressed) in [true, false].into_iter().enumerate() {
            let mut out = ctx.run_ui(
                make_input(
                    viewport,
                    Some(0.10 + 0.02 * i as f64),
                    vec![
                        egui::Event::PointerMoved(update_btn),
                        egui::Event::PointerButton {
                            pos: update_btn,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                ),
                |ui| {
                    action = screen.show(ui, &mut settings, false);
                },
            );
            out.textures_delta.clear();
        }
        assert_eq!(action, Some(ConnectAction::StartUpdate));

        // Settings window shows build_display + Check for updates button emitting ToolbarAction::CheckForUpdates.
        let sctx = egui::Context::default();
        let mut open = true;
        let mut sw = sctx.run_ui(make_input(viewport, Some(0.0), Vec::new()), |ui| {
            let _ = settings_window_full(
                ui.ctx(),
                &mut open,
                &mut settings,
                None,
                Some("v0.2.0 (26e488f)"),
                Some(&UpdateUiState::Idle),
            );
        });
        sw.textures_delta.clear();
        sw = sctx.run_ui(make_input(viewport, Some(0.02), Vec::new()), |ui| {
            let _ = settings_window_full(
                ui.ctx(),
                &mut open,
                &mut settings,
                None,
                Some("v0.2.0 (26e488f)"),
                Some(&UpdateUiState::Idle),
            );
        });
        sw.textures_delta.clear();
        assert!(
            shapes_contain_text(&sw.shapes, "v0.2.0 (26e488f)"),
            "Settings window must display build_display ({lang:?})"
        );
        let check_btn = sw
            .shapes
            .iter()
            .find_map(|c| find_text_rect(&c.shape, t.update_check_btn))
            .unwrap_or_else(|| panic!("Check for updates button missing in Settings ({lang:?})"))
            .center();
        let mut s_actions = Vec::new();
        for (i, pressed) in [true, false].into_iter().enumerate() {
            let mut out = sctx.run_ui(
                make_input(
                    viewport,
                    Some(0.10 + 0.02 * i as f64),
                    vec![
                        egui::Event::PointerMoved(check_btn),
                        egui::Event::PointerButton {
                            pos: check_btn,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                ),
                |ui| {
                    s_actions = settings_window_full(
                        ui.ctx(),
                        &mut open,
                        &mut settings,
                        None,
                        Some("v0.2.0 (26e488f)"),
                        Some(&UpdateUiState::Idle),
                    );
                },
            );
            out.textures_delta.clear();
        }
        assert!(
            s_actions.contains(&ToolbarAction::CheckForUpdates),
            "clicking Check for updates in Settings must emit ToolbarAction::CheckForUpdates, got {s_actions:?}"
        );
    }
}
