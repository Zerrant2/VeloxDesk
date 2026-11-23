//! Performance HUD (ARCHITECTURE §8): one-line per-stage latency summary drawn with egui
//! on top of the video, in the same render pass. Toggle: Ctrl+Shift+H.

use veloxdesk_core_traits::metrics::HudSnapshot;
use veloxdesk_core_traits::transport::PathKind;
use winit::keyboard::{KeyCode, ModifiersState};

/// Localized static labels for the Performance HUD overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HudLabels {
    pub capture: &'static str,
    pub encode: &'static str,
    pub net: &'static str,
    pub rtt: &'static str,
    pub decode: &'static str,
    pub render: &'static str,
    pub total: &'static str,
    pub ms: &'static str,
    pub fps: &'static str,
    pub mbps: &'static str,
    pub loss: &'static str,
    pub fec_rec: &'static str,
    pub decode_prefix: &'static str,
    pub zero_copy: &'static str,
    pub yes: &'static str,
    pub no: &'static str,
    pub path_unknown: &'static str,
    pub path_direct: &'static str,
    pub path_p2p: &'static str,
    pub path_relay: &'static str,
}

impl HudLabels {
    pub const EN: Self = Self {
        capture: "Capture",
        encode: "Encode",
        net: "Net",
        rtt: "RTT",
        decode: "Decode",
        render: "Render",
        total: "Total",
        ms: "ms",
        fps: "fps",
        mbps: "Mb/s",
        loss: "loss",
        fec_rec: "FEC rec",
        decode_prefix: "decode",
        zero_copy: "zero-copy",
        yes: "yes",
        no: "no",
        path_unknown: "Unknown",
        path_direct: "Direct",
        path_p2p: "P2P Direct",
        path_relay: "Relay",
    };

    pub fn path_label(&self, path: PathKind) -> &'static str {
        match path {
            PathKind::Unknown => self.path_unknown,
            PathKind::Direct => self.path_direct,
            PathKind::P2p => self.path_p2p,
            PathKind::Relay => self.path_relay,
        }
    }
}

impl Default for HudLabels {
    fn default() -> Self {
        Self::EN
    }
}

fn ms(us: u32) -> String {
    format!("{:.1}", us as f64 / 1000.0)
}

/// Format per-stage latency timings using `l`.
pub fn timings_line_with(s: &HudSnapshot, l: &HudLabels) -> String {
    let t = &s.timings;
    format!(
        "{} {} | {} {} | {} {} ({} {}) | {} {} | {} {} | {} {} {}",
        l.capture,
        ms(t.capture_us.saturating_add(t.convert_us)),
        l.encode,
        ms(t.encode_us),
        l.net,
        ms(t.network_us),
        l.rtt,
        ms(s.rtt_us),
        l.decode,
        ms(t.decode_us),
        l.render,
        ms(t.render_us),
        l.total,
        ms(t.total_us),
        l.ms,
    )
}

/// `Capture 0.6 | Encode 2.1 | Net 0.9 (RTT 0.4) | Decode 1.2 | Render 0.8 | Total 5.6 ms`
pub fn timings_line(s: &HudSnapshot) -> String {
    timings_line_with(s, &HudLabels::EN)
}

/// Format FPS, codec, bitrate, loss, FEC, and transport path using `l`.
pub fn stats_line_with(s: &HudSnapshot, l: &HudLabels) -> String {
    let encoder = if s.encoder.is_empty() { "?" } else { s.encoder.as_str() };
    let codec = if s.codec.is_empty() { "?" } else { s.codec.as_str() };
    format!(
        "{:.0} {} · {} {} {} · {:.1} {} · {} {:.1}% · {} {:.0}% · {}",
        s.fps,
        l.fps,
        encoder,
        codec,
        if s.hw_encode { "HW" } else { "SW" },
        s.bitrate_kbps as f64 / 1000.0,
        l.mbps,
        l.loss,
        s.packet_loss_pct,
        l.fec_rec,
        s.fec_recovery_pct,
        l.path_label(s.path),
    )
}

/// `120 fps · NVENC H.265 HW · 38.2 Mb/s · loss 0.3% · FEC rec 100% · P2P Direct`
pub fn stats_line(s: &HudSnapshot) -> String {
    stats_line_with(s, &HudLabels::EN)
}

/// Decoder / presentation details using `l`.
pub fn detail_line_with(s: &HudSnapshot, l: &HudLabels) -> String {
    let decoder = if s.decoder.is_empty() { "?" } else { s.decoder.as_str() };
    format!(
        "{}x{} · {} {} {} · {} {}",
        s.resolution.0,
        s.resolution.1,
        l.decode_prefix,
        decoder,
        if s.hw_decode { "HW" } else { "SW" },
        l.zero_copy,
        if s.zero_copy { l.yes } else { l.no },
    )
}

/// Decoder / presentation details.
pub fn detail_line(s: &HudSnapshot) -> String {
    detail_line_with(s, &HudLabels::EN)
}

/// Full single-line form used in logs.
pub fn hud_line(s: &HudSnapshot) -> String {
    format!("{} · {}", timings_line(s), stats_line(s))
}

/// Whether a key press is the HUD toggle (Ctrl+Shift+H).
pub fn is_toggle_shortcut(key: KeyCode, mods: ModifiersState) -> bool {
    key == KeyCode::KeyH && mods.control_key() && mods.shift_key() && !mods.alt_key()
}

/// Draw the HUD in the top-left corner with localized labels.
pub fn show_hud_with(ui: &mut egui::Ui, s: &HudSnapshot, extra: Option<&str>, labels: &HudLabels) {
    egui::Area::new(egui::Id::unique("veloxdesk-hud"))
        .anchor(egui::Align2::LEFT_TOP, egui::vec2(8.0, 8.0))
        .interactable(false)
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            egui::Frame::new()
                .fill(egui::Color32::from_black_alpha(170))
                .corner_radius(4.0)
                .inner_margin(egui::Margin::symmetric(8, 4))
                .show(ui, |ui| {
                    let color = egui::Color32::from_rgb(120, 255, 140);
                    let font = egui::FontId::monospace(12.0);
                    for line in [
                        timings_line_with(s, labels),
                        stats_line_with(s, labels),
                        detail_line_with(s, labels),
                    ] {
                        ui.label(egui::RichText::new(line).font(font.clone()).color(color));
                    }
                    if let Some(e) = extra {
                        ui.label(egui::RichText::new(e).font(font).color(egui::Color32::LIGHT_GRAY));
                    }
                });
        });
}

/// Draw the HUD in the top-left corner (English labels).
pub fn show_hud(ui: &mut egui::Ui, s: &HudSnapshot, extra: Option<&str>) {
    show_hud_with(ui, s, extra, &HudLabels::EN);
}

#[cfg(test)]
mod tests {
    use super::*;
    use veloxdesk_core_traits::metrics::StageTimings;
    use veloxdesk_core_traits::transport::PathKind;

    #[test]
    fn hud_format_matches_spec() {
        let s = HudSnapshot {
            timings: StageTimings {
                capture_us: 400,
                convert_us: 200,
                encode_us: 2100,
                network_us: 900,
                decode_us: 1200,
                render_us: 800,
                total_us: 5600,
            },
            rtt_us: 400,
            fps: 120.0,
            codec: "H.265".into(),
            encoder: "NVENC".into(),
            hw_encode: true,
            bitrate_kbps: 38_200,
            packet_loss_pct: 0.3,
            fec_recovery_pct: 100.0,
            path: PathKind::P2p,
            ..Default::default()
        };
        assert_eq!(
            hud_line(&s),
            "Capture 0.6 | Encode 2.1 | Net 0.9 (RTT 0.4) | Decode 1.2 | Render 0.8 | Total 5.6 ms · \
             120 fps · NVENC H.265 HW · 38.2 Mb/s · loss 0.3% · FEC rec 100% · P2P Direct"
        );
    }

    #[test]
    fn shortcut() {
        let m = ModifiersState::CONTROL | ModifiersState::SHIFT;
        assert!(is_toggle_shortcut(KeyCode::KeyH, m));
        assert!(!is_toggle_shortcut(KeyCode::KeyH, ModifiersState::CONTROL));
        assert!(!is_toggle_shortcut(KeyCode::KeyJ, m));
    }

    #[test]
    fn hud_with_custom_labels_renders_localized_lines() {
        let ru = HudLabels {
            capture: "Захват",
            encode: "Кодир.",
            net: "Сеть",
            rtt: "RTT",
            decode: "Декод.",
            render: "Рендер",
            total: "Итого",
            ms: "мс",
            fps: "к/с",
            mbps: "Мбит/с",
            loss: "потери",
            fec_rec: "FEC восст.",
            decode_prefix: "декодер",
            zero_copy: "zero-copy",
            yes: "да",
            no: "нет",
            path_unknown: "Неизвестно",
            path_direct: "Прямое",
            path_p2p: "P2P прямое",
            path_relay: "Релей",
        };
        let s = HudSnapshot {
            timings: StageTimings {
                capture_us: 400,
                convert_us: 200,
                encode_us: 2100,
                network_us: 900,
                decode_us: 1200,
                render_us: 800,
                total_us: 5600,
            },
            rtt_us: 400,
            fps: 120.0,
            codec: "H.265".into(),
            encoder: "NVENC".into(),
            decoder: "NVDEC".into(),
            hw_encode: true,
            hw_decode: true,
            zero_copy: true,
            resolution: (1920, 1080),
            bitrate_kbps: 38_200,
            packet_loss_pct: 0.3,
            fec_recovery_pct: 100.0,
            path: PathKind::P2p,
        };
        let t_line = timings_line_with(&s, &ru);
        let s_line = stats_line_with(&s, &ru);
        let d_line = detail_line_with(&s, &ru);
        assert!(t_line.contains("Захват") && t_line.contains("Итого 5.6 мс"));
        assert!(s_line.contains("120 к/с") && s_line.contains("Мбит/с") && s_line.contains("P2P прямое"));
        assert!(d_line.contains("декодер NVDEC HW") && d_line.contains("zero-copy да"));

        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0))),
                ..Default::default()
            },
            |ui| {
                show_hud_with(ui, &s, Some("курсор захвачен (Ctrl+Alt+G)"), &ru);
            },
        );
        out.textures_delta.clear();

        fn verify_glyphs(ctx: &egui::Context, shape: &egui::epaint::Shape) {
            match shape {
                egui::epaint::Shape::Vec(v) => v.iter().for_each(|x| verify_glyphs(ctx, x)),
                egui::epaint::Shape::Text(ts) => {
                    for sec in &ts.galley.job.sections {
                        let slice = &ts.galley.job.text[sec.byte_range.start.0..sec.byte_range.end.0];
                        assert!(
                            ctx.fonts_mut(|f| f.has_glyphs(&sec.format.font_id, slice)),
                            "missing glyph in HUD text {slice:?}"
                        );
                    }
                }
                _ => {}
            }
        }
        for c in &out.shapes {
            verify_glyphs(&ctx, &c.shape);
        }
    }
}
