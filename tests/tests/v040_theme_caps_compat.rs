//! VeloxDesk v0.4.0 Integration Tests:
//! - Desktop Dark / Light / System themes (`ThemePreference`) JSON persistence, v0.3.0 compat,
//!   and headless `egui` rendering in BOTH Dark & Light visuals across EN & RU with 100% font-glyph checks.
//! - OTP Auto-CAPS `PasswordPrompt` behavior across OTP-only, Both, and Permanent-only host modes
//!   (guaranteeing mixed-case permanent passwords are never corrupted).
//! - Android Host session navigation pills (`Back` / `Home` / `Recents`) in `SessionToolbar`.
//! - Dynamic portrait ↔ landscape display aspect ratio (`compute_video_rect` / `window_to_video`
//!   with `ScaleMode::Fit` on `1080x2400` and `2400x1080`).
//! - Automated source scan verifying zero forbidden missing-glyph Unicode icons across
//!   `crates/ui/src` and `crates/render/src` + complete v0.4.0 EN/RU i18n coverage.

use std::path::{Path, PathBuf};

use veloxdesk_core_traits::render::ScaleMode;
use veloxdesk_protocol::control::{DisplayInfo, Permissions};
use veloxdesk_render::layout::{compute_video_rect, window_to_video};
use veloxdesk_ui::{
    AcceptPrompt, ConnectAction, ConnectScreen, IncomingSessionInfo, Language, PasswordMode, PasswordPrompt,
    SavedTrustedPeer, SessionInfo, SessionToolbar, Settings, TailscaleInfo, TailscalePeer, ThemePreference,
    ToolbarAction, UpdateUiState, settings_window_with_security, tr,
};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tests crate must live under workspace root")
        .to_path_buf()
}

fn make_input(viewport: egui::Vec2, time: f64, events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, viewport)),
        time: Some(time),
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
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

fn strip_tests_and_comments(src: &str) -> String {
    let prod = src.split("\n#[cfg(test)]").next().unwrap_or(src);
    let mut out = String::with_capacity(prod.len());
    for line in prod.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            out.push('\n');
            continue;
        }
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

fn click_at(ctx: &egui::Context, viewport: egui::Vec2, base_time: f64, pos: egui::Pos2, mut f: impl FnMut(&mut egui::Ui)) {
    for (i, pressed) in [true, false].into_iter().enumerate() {
        let mut out = ctx.run_ui(
            make_input(
                viewport,
                base_time + 0.02 * i as f64,
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
            &mut f,
        );
        out.textures_delta.clear();
    }
}

// ---------------------------------------------------------------------------
// 1. ThemePreference persistence + v0.3.0 backwards compatibility
// ---------------------------------------------------------------------------

#[test]
fn v040_theme_preference_persistence_and_v030_compat() {
    assert_eq!(Settings::default().theme, ThemePreference::System);
    assert_eq!(ThemePreference::System.next(), ThemePreference::Dark);
    assert_eq!(ThemePreference::Dark.next(), ThemePreference::Light);
    assert_eq!(ThemePreference::Light.next(), ThemePreference::System);
    assert_eq!(ThemePreference::System.to_egui(), egui::ThemePreference::System);
    assert_eq!(ThemePreference::Dark.to_egui(), egui::ThemePreference::Dark);
    assert_eq!(ThemePreference::Light.to_egui(), egui::ThemePreference::Light);

    // v0.3.0 settings JSON without `theme` field deserializes cleanly with default `System`.
    let v030_json = r#"{
        "language": "Ru",
        "recent": ["10.0.0.5:21118"],
        "rendezvous_server": "rdv.example.com:21116",
        "mode": "Gaming",
        "require_accept": true,
        "password_mode": "Both",
        "permanent_password_hash": null,
        "trusted_peers": []
    }"#;
    let migrated: Settings = serde_json::from_str(v030_json).expect("v0.3.0 JSON must deserialize");
    assert_eq!(migrated.theme, ThemePreference::System);
    assert_eq!(migrated.language, Language::Ru);
    assert_eq!(migrated.password_mode, PasswordMode::Both);

    // Disk JSON round-trip for every ThemePreference variant.
    let dir = std::env::temp_dir().join(format!("veloxdesk-v040-theme-{}", std::process::id()));
    for th in ThemePreference::ALL {
        let path = dir.join(format!("settings-{th:?}.json"));
        let s = Settings { theme: th, language: Language::Ru, ..Default::default() };
        s.save(&path).expect("save settings");
        let loaded = Settings::load(&path).expect("load settings");
        assert_eq!(loaded.theme, th);
        assert_eq!(loaded.language, Language::Ru);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// 2. Headless egui render in BOTH Dark & Light visuals (EN & RU) + Android nav
// ---------------------------------------------------------------------------

#[test]
fn v040_headless_render_dark_and_light_en_ru_with_glyphs_and_android_nav() {
    let viewport = egui::vec2(1280.0, 800.0);
    let dir = std::env::temp_dir().join(format!("veloxdesk-v040-ui-{}", std::process::id()));

    for theme in [ThemePreference::Dark, ThemePreference::Light] {
        let expect_dark = matches!(theme, ThemePreference::Dark);

        for lang in Language::ALL {
            let t = tr(lang);
            let ctx = egui::Context::default();
            ctx.set_theme(theme.to_egui());

            let settings_path = dir.join(format!("settings-{theme:?}-{lang:?}.json"));
            let mut settings = Settings {
                language: lang,
                theme,
                rendezvous_server: "rdv.example.com:21116".into(),
                password_mode: PasswordMode::Both,
                permanent_password_hash: Some(
                    "$argon2id$v=19$m=1024,t=1,p=1$c2FsdHNhbHRzYWx0$aGFzaGhhc2hoYXNo".into(),
                ),
                trusted_peers: vec![SavedTrustedPeer {
                    public_key_hex: "bb".repeat(32),
                    device_id: "123456789".into(),
                    name: "Pixel-9".into(),
                    fingerprint: "0123456789abcdef0123456789abcdef".into(),
                    ..Default::default()
                }],
                ..Default::default()
            };
            settings.recent.push("192.168.1.50:21118".into());

            // A. ConnectScreen in Dark / Light + Update banner + Tailscale peers + Theme click test.
            let mut screen = ConnectScreen::new(&settings);
            screen.settings_path = Some(settings_path.clone());
            screen.device_id = "987654321".into();
            screen.host_running = true;
            screen.active_sessions = 1;
            screen.host_otp = "AB12CD34".into();
            screen.show_otp = true;
            screen.build_display = "v0.4.0 (dev)".into();
            screen.update_state = UpdateUiState::Available {
                current: "0.4.0".into(),
                latest: "0.5.0".into(),
                notes: "Release notes".into(),
                html_url: "https://github.com/Zerrant2/VeloxDesk/releases/tag/v0.5.0".into(),
            };
            screen.tailscale = TailscaleInfo {
                installed: true,
                running: true,
                self_ip: Some("100.64.0.10".into()),
                self_name: Some("phone".into()),
                peers: vec![TailscalePeer {
                    name: "desktop-pc".into(),
                    ip: "100.64.0.20".into(),
                    os: "windows".into(),
                    online: true,
                }],
                error: None,
            };

            let mut observed_dark = None;
            let mut out_connect = ctx.run_ui(make_input(viewport, 0.0, Vec::new()), |ui| {
                observed_dark = Some(ui.visuals().dark_mode);
                let _ = screen.show(ui, &mut settings, false);
            });
            out_connect.textures_delta.clear();
            assert_eq!(
                observed_dark,
                Some(expect_dark),
                "egui visuals().dark_mode must match {theme:?}"
            );
            for c in &out_connect.shapes {
                verify_text_shapes_have_glyphs(&ctx, &c.shape);
            }

            // Clicking the opposite theme pill on ConnectScreen emits ConnectAction::SetTheme and persists.
            let next_theme = if expect_dark { ThemePreference::Light } else { ThemePreference::Dark };
            let next_label = next_theme.label_for(lang);
            let theme_btn_pos = out_connect
                .shapes
                .iter()
                .find_map(|c| find_text_rect(&c.shape, next_label))
                .unwrap_or_else(|| panic!("theme button {next_label:?} missing on ConnectScreen ({lang:?})"))
                .center();
            let mut emitted_connect = None;
            click_at(&ctx, viewport, 0.10, theme_btn_pos, |ui| {
                if let Some(act) = screen.show(ui, &mut settings, false) {
                    emitted_connect = Some(act);
                }
            });
            assert_eq!(emitted_connect, Some(ConnectAction::SetTheme(next_theme)));
            assert_eq!(Settings::load(&settings_path).unwrap().theme, next_theme);
            // Restore theme for subsequent component renders in this loop iteration.
            settings.theme = theme;

            // B. Settings window (with Theme row + Host Access & Security) + PasswordPrompt + AcceptPrompt.
            let sctx = egui::Context::default();
            sctx.set_theme(theme.to_egui());
            let mut open = true;
            let mut pw = PasswordPrompt::new_with_methods(
                "987654321".into(),
                "0123456789abcdef0123456789abcdef".into(),
                t.auth_methods_both.into(),
                true,
                true,
            );
            let mut accept = AcceptPrompt::new(
                IncomingSessionInfo {
                    peer_device_id: "123456789".into(),
                    peer_name: "Pixel-9".into(),
                    peer_fingerprint: "abcdef0123456789abcdef0123456789".into(),
                    peer_public_key: [5u8; 32],
                },
                Permissions { view: true, input: true, ..Default::default() },
            );
            let mut last_settings_shapes = Vec::new();
            for step in 0..2 {
                let mut out = sctx.run_ui(make_input(viewport, 0.02 * step as f64, Vec::new()), |ui| {
                    assert_eq!(ui.visuals().dark_mode, expect_dark);
                    let _ = settings_window_with_security(
                        ui.ctx(),
                        &mut open,
                        &mut settings,
                        None,
                        Some("v0.4.0 (dev)"),
                        Some(&UpdateUiState::Idle),
                        Some(t.pw_saved_notice),
                    );
                    let _ = pw.show_for(ui.ctx(), lang);
                    let _ = accept.show_for(ui.ctx(), lang);
                });
                out.textures_delta.clear();
                for c in &out.shapes {
                    verify_text_shapes_have_glyphs(&sctx, &c.shape);
                }
                last_settings_shapes = out.shapes;
            }
            assert!(
                shapes_contain_text(&last_settings_shapes, t.settings_theme_label),
                "Settings window missing Theme row ({theme:?}, {lang:?})"
            );

            // C. SessionToolbar: Android host shows Back / Home / Recents and emits actions; Windows host hides them.
            let tctx_win = egui::Context::default();
            tctx_win.set_theme(theme.to_egui());
            let win_info = SessionInfo {
                host_name: "Windows-PC".into(),
                host_os: "windows".into(),
                displays: vec![DisplayInfo {
                    index: 0,
                    name: "Main".into(),
                    width: 1920,
                    height: 1080,
                    primary: true,
                    ..Default::default()
                }],
                ..Default::default()
            };
            let mut tb_win = SessionToolbar { expanded: true, ..Default::default() };
            let mut out_win = tctx_win.run_ui(make_input(viewport, 0.0, Vec::new()), |ui| {
                let _ = tb_win.show(ui, &win_info, &mut settings);
            });
            out_win.textures_delta.clear();
            out_win = tctx_win.run_ui(make_input(viewport, 0.02, Vec::new()), |ui| {
                let _ = tb_win.show(ui, &win_info, &mut settings);
            });
            out_win.textures_delta.clear();
            assert!(
                !shapes_contain_text(&out_win.shapes, t.toolbar_android_recents),
                "Windows host toolbar must NOT show Android Recents button"
            );

            let tctx_and = egui::Context::default();
            tctx_and.set_theme(theme.to_egui());
            let and_info = SessionInfo {
                host_name: "Pixel-9".into(),
                host_os: "android".into(),
                displays: vec![DisplayInfo {
                    index: 0,
                    name: "Android".into(),
                    width: 1080,
                    height: 2400,
                    primary: true,
                    ..Default::default()
                }],
                ..Default::default()
            };
            let mut tb_and = SessionToolbar { expanded: true, ..Default::default() };
            let mut out_and = tctx_and.run_ui(make_input(viewport, 0.0, Vec::new()), |ui| {
                let _ = tb_and.show(ui, &and_info, &mut settings);
            });
            out_and.textures_delta.clear();
            out_and = tctx_and.run_ui(make_input(viewport, 0.02, Vec::new()), |ui| {
                let _ = tb_and.show(ui, &and_info, &mut settings);
            });
            out_and.textures_delta.clear();
            for c in &out_and.shapes {
                verify_text_shapes_have_glyphs(&tctx_and, &c.shape);
            }

            for (idx, (label, expected_action)) in [
                (t.toolbar_android_back, ToolbarAction::AndroidBack),
                (t.toolbar_android_home, ToolbarAction::AndroidHome),
                (t.toolbar_android_recents, ToolbarAction::AndroidRecents),
            ]
            .into_iter()
            .enumerate()
            {
                let pos = out_and
                    .shapes
                    .iter()
                    .find_map(|c| find_text_rect(&c.shape, label))
                    .unwrap_or_else(|| panic!("Android nav button {label:?} missing ({theme:?}, {lang:?})"))
                    .center();
                let mut actions = Vec::new();
                click_at(&tctx_and, viewport, 0.10 + 0.08 * idx as f64, pos, |ui| {
                    let a = tb_and.show(ui, &and_info, &mut settings);
                    if !a.is_empty() {
                        actions = a;
                    }
                });
                assert_eq!(actions, vec![expected_action], "Android nav {label:?} ({theme:?}, {lang:?})");
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// 3. OTP Auto-CAPS across OTP-only, Both, and Permanent-only host modes
// ---------------------------------------------------------------------------

#[test]
fn v040_otp_auto_caps_and_mixed_case_permanent_password() {
    let viewport = egui::vec2(1280.0, 720.0);

    for lang in Language::ALL {
        let t = tr(lang);

        // 1. OTP-only (`allow_otp = true, allow_perm = false`):
        //    defaults uppercase_mode = true, shows CAPS pill, uppercases & strips spaces.
        let mut otp_only = PasswordPrompt::new_with_methods(
            "123456789".into(),
            "0123456789abcdef".into(),
            t.auth_methods_otp.into(),
            true,
            false,
        );
        assert!(otp_only.uppercase_mode, "OTP-only must default uppercase_mode=true");
        otp_only.password = "ab12 cd34\t ".into();
        let ctx_otp = egui::Context::default();
        let mut out_otp = ctx_otp.run_ui(make_input(viewport, 0.0, Vec::new()), |ui| {
            let _ = otp_only.show_for(ui.ctx(), lang);
        });
        out_otp.textures_delta.clear();
        out_otp = ctx_otp.run_ui(make_input(viewport, 0.02, Vec::new()), |ui| {
            let _ = otp_only.show_for(ui.ctx(), lang);
        });
        out_otp.textures_delta.clear();
        assert_eq!(otp_only.password, "AB12CD34");
        assert!(
            shapes_contain_text(&out_otp.shapes, t.caps_btn),
            "CAPS pill must be shown for OTP-only ({lang:?})"
        );

        // 2. Permanent-only (`allow_otp = false, allow_perm = true`):
        //    defaults uppercase_mode = false, hides CAPS pill, preserves mixed-case + spaces verbatim.
        let mut perm_only = PasswordPrompt::new_with_methods(
            "123456789".into(),
            "0123456789abcdef".into(),
            t.auth_methods_perm.into(),
            false,
            true,
        );
        assert!(!perm_only.uppercase_mode, "Permanent-only must default uppercase_mode=false");
        perm_only.password = "MyMixedCase Pass!99".into();
        let ctx_perm = egui::Context::default();
        let mut out_perm = ctx_perm.run_ui(make_input(viewport, 0.0, Vec::new()), |ui| {
            let _ = perm_only.show_for(ui.ctx(), lang);
        });
        out_perm.textures_delta.clear();
        out_perm = ctx_perm.run_ui(make_input(viewport, 0.02, Vec::new()), |ui| {
            let _ = perm_only.show_for(ui.ctx(), lang);
        });
        out_perm.textures_delta.clear();
        assert_eq!(perm_only.password, "MyMixedCase Pass!99");
        assert!(
            !shapes_contain_text(&out_perm.shapes, t.caps_btn),
            "CAPS pill must be hidden for Permanent-only ({lang:?})"
        );

        // 3. Both (`allow_otp = true, allow_perm = true`):
        //    defaults uppercase_mode = false (never breaks mixed-case permanent passwords),
        //    shows toggleable CAPS pill which enables uppercase_mode & normalizes when clicked.
        let mut both = PasswordPrompt::new_with_methods(
            "123456789".into(),
            "0123456789abcdef".into(),
            t.auth_methods_both.into(),
            true,
            true,
        );
        assert!(!both.uppercase_mode, "Both mode must default uppercase_mode=false");
        both.password = "Secret Pass42".into();
        let ctx_both = egui::Context::default();
        let mut out_both = ctx_both.run_ui(make_input(viewport, 0.0, Vec::new()), |ui| {
            let _ = both.show_for(ui.ctx(), lang);
        });
        out_both.textures_delta.clear();
        out_both = ctx_both.run_ui(make_input(viewport, 0.02, Vec::new()), |ui| {
            let _ = both.show_for(ui.ctx(), lang);
        });
        out_both.textures_delta.clear();
        assert_eq!(both.password, "Secret Pass42", "Both mode must preserve mixed-case until CAPS clicked");

        let caps_pos = out_both
            .shapes
            .iter()
            .find_map(|c| find_text_rect(&c.shape, t.caps_btn))
            .unwrap_or_else(|| panic!("CAPS pill missing in Both mode ({lang:?})"))
            .center();
        click_at(&ctx_both, viewport, 0.10, caps_pos, |ui| {
            let _ = both.show_for(ui.ctx(), lang);
        });
        assert!(both.uppercase_mode, "clicking CAPS in Both mode must enable uppercase_mode");
        assert_eq!(both.password, "SECRETPASS42");
    }
}

// ---------------------------------------------------------------------------
// 4. Portrait <-> Landscape dynamic display aspect ratio (`ScaleMode::Fit`)
// ---------------------------------------------------------------------------

#[test]
fn v040_portrait_landscape_android_display_aspect_ratio_fit() {
    let win = (1920, 1080);

    // Portrait phone (1080x2400) inside 16:9 desktop window -> centered pillarbox (486x1080 at x=717).
    let portrait = (1080, 2400);
    let rp = compute_video_rect(ScaleMode::Fit, portrait, win);
    assert_eq!((rp.x, rp.y, rp.w, rp.h), (717.0, 0.0, 486.0, 1080.0));
    assert_eq!(window_to_video(&rp, portrait, (100.0, 540.0)), None);
    assert_eq!(window_to_video(&rp, portrait, (1800.0, 540.0)), None);
    let (px, py) = window_to_video(&rp, portrait, (960.0, 540.0)).expect("portrait center must hit video");
    assert!((px - 540).abs() <= 2 && (py - 1200).abs() <= 2, "portrait center mapped to ({px}, {py})");

    // Rotated landscape phone (2400x1080) inside same window -> centered letterbox (1920x864 at y=108).
    let landscape = (2400, 1080);
    let rl = compute_video_rect(ScaleMode::Fit, landscape, win);
    assert_eq!((rl.x, rl.y, rl.w, rl.h), (0.0, 108.0, 1920.0, 864.0));
    assert_eq!(window_to_video(&rl, landscape, (960.0, 20.0)), None);
    assert_eq!(window_to_video(&rl, landscape, (960.0, 1050.0)), None);
    let (lx, ly) = window_to_video(&rl, landscape, (960.0, 540.0)).expect("landscape center must hit video");
    assert!((lx - 1200).abs() <= 2 && (ly - 540).abs() <= 2, "landscape center mapped to ({lx}, {ly})");
}

// ---------------------------------------------------------------------------
// 5. Zero forbidden missing-glyph Unicode icons + v0.4.0 EN/RU i18n parity
// ---------------------------------------------------------------------------

#[test]
fn v040_zero_forbidden_unicode_icons_and_i18n_parity() {
    const FORBIDDEN: &[char] = &[
        '\u{25CF}', '\u{25CB}', '\u{2715}', '\u{2716}', '\u{2699}', '\u{1F512}', '\u{1F513}', '\u{1F4CB}',
        '\u{1F4C1}', '\u{1F5A5}', '\u{1F3AE}', '\u{1F5D1}', '\u{26A0}', '\u{2713}', '\u{2714}', '\u{2717}',
        '\u{1F504}', '\u{26A1}', '\u{2139}', '\u{1F50A}', '\u{1F507}', '\u{1F3A4}', '\u{25B6}', '\u{23F8}',
        '\u{23F9}', '\u{26F6}', '\u{2605}', '\u{2606}', '\u{25C0}', '\u{25B2}', '\u{25BC}', '\u{25BE}',
        '\u{25B4}', '\u{2B06}', '\u{2192}',
    ];

    let root = workspace_root();
    let mut files = Vec::new();
    collect_rs_files(&root.join("crates/ui/src"), &mut files);
    collect_rs_files(&root.join("crates/render/src"), &mut files);

    for path in files {
        let raw = std::fs::read_to_string(&path).expect("read ui/render src");
        let code = strip_tests_and_comments(&raw);
        for (idx, line) in code.lines().enumerate() {
            for &ch in FORBIDDEN {
                assert!(
                    !line.contains(ch),
                    "{}:{} contains forbidden missing-glyph U+{:04X}: {}",
                    path.display(),
                    idx + 1,
                    ch as u32,
                    line.trim()
                );
            }
        }
    }

    let ctx = egui::Context::default();
    let mut init = ctx.run_ui(egui::RawInput::default(), |_| {});
    init.textures_delta.clear();

    let en = tr(Language::En);
    let ru = tr(Language::Ru);
    let v040_pairs = [
        ("theme_system", en.theme_system, ru.theme_system, true),
        ("theme_dark", en.theme_dark, ru.theme_dark, true),
        ("theme_light", en.theme_light, ru.theme_light, true),
        ("settings_theme_label", en.settings_theme_label, ru.settings_theme_label, true),
        ("tip_theme", en.tip_theme, ru.tip_theme, true),
        ("theme_switch_tooltip", en.theme_switch_tooltip, ru.theme_switch_tooltip, true),
        ("caps_btn", en.caps_btn, ru.caps_btn, false),
        ("tip_caps", en.tip_caps, ru.tip_caps, true),
        ("toolbar_android_back", en.toolbar_android_back, ru.toolbar_android_back, true),
        ("toolbar_android_home", en.toolbar_android_home, ru.toolbar_android_home, true),
        ("toolbar_android_recents", en.toolbar_android_recents, ru.toolbar_android_recents, true),
    ];
    for (name, e, r, expect_cyrillic) in v040_pairs {
        assert!(!e.trim().is_empty(), "EN.{name} empty");
        assert!(!r.trim().is_empty(), "RU.{name} empty");
        if expect_cyrillic {
            assert_ne!(e, r, "{name} must differ between EN and RU");
            assert!(
                r.chars().any(|c| ('\u{0400}'..='\u{04FF}').contains(&c)),
                "RU.{name} must contain Cyrillic: {r:?}"
            );
        }
        assert!(ctx.fonts_mut(|f| f.has_glyphs(&egui::FontId::proportional(14.0), e)));
        assert!(ctx.fonts_mut(|f| f.has_glyphs(&egui::FontId::proportional(14.0), r)));
    }
}
