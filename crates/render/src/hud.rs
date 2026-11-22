//! Performance HUD (ARCHITECTURE §8): one-line per-stage latency summary drawn with egui
//! on top of the video, in the same render pass. Toggle: Ctrl+Shift+H.

use veloxdesk_core_traits::metrics::HudSnapshot;
use winit::keyboard::{KeyCode, ModifiersState};

fn ms(us: u32) -> String {
    format!("{:.1}", us as f64 / 1000.0)
}

/// `Capture 0.6 | Encode 2.1 | Net 0.9 (RTT 0.4) | Decode 1.2 | Render 0.8 | Total 5.6 ms`
pub fn timings_line(s: &HudSnapshot) -> String {
    let t = &s.timings;
    format!(
        "Capture {} | Encode {} | Net {} (RTT {}) | Decode {} | Render {} | Total {} ms",
        ms(t.capture_us.saturating_add(t.convert_us)),
        ms(t.encode_us),
        ms(t.network_us),
        ms(s.rtt_us),
        ms(t.decode_us),
        ms(t.render_us),
        ms(t.total_us),
    )
}

/// `120 fps · NVENC H.265 HW · 38.2 Mb/s · loss 0.3% · FEC rec 100% · P2P Direct`
pub fn stats_line(s: &HudSnapshot) -> String {
    let encoder = if s.encoder.is_empty() { "?" } else { s.encoder.as_str() };
    let codec = if s.codec.is_empty() { "?" } else { s.codec.as_str() };
    format!(
        "{:.0} fps · {} {} {} · {:.1} Mb/s · loss {:.1}% · FEC rec {:.0}% · {}",
        s.fps,
        encoder,
        codec,
        if s.hw_encode { "HW" } else { "SW" },
        s.bitrate_kbps as f64 / 1000.0,
        s.packet_loss_pct,
        s.fec_recovery_pct,
        s.path.label(),
    )
}

/// Decoder / presentation details.
pub fn detail_line(s: &HudSnapshot) -> String {
    let decoder = if s.decoder.is_empty() { "?" } else { s.decoder.as_str() };
    format!(
        "{}x{} · decode {} {} · zero-copy {}",
        s.resolution.0,
        s.resolution.1,
        decoder,
        if s.hw_decode { "HW" } else { "SW" },
        if s.zero_copy { "yes" } else { "no" },
    )
}

/// Full single-line form used in logs.
pub fn hud_line(s: &HudSnapshot) -> String {
    format!("{} · {}", timings_line(s), stats_line(s))
}

/// Whether a key press is the HUD toggle (Ctrl+Shift+H).
pub fn is_toggle_shortcut(key: KeyCode, mods: ModifiersState) -> bool {
    key == KeyCode::KeyH && mods.control_key() && mods.shift_key() && !mods.alt_key()
}

/// Draw the HUD in the top-left corner.
pub fn show_hud(ui: &mut egui::Ui, s: &HudSnapshot, extra: Option<&str>) {
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
                    for line in [timings_line(s), stats_line(s), detail_line(s)] {
                        ui.label(egui::RichText::new(line).font(font.clone()).color(color));
                    }
                    if let Some(e) = extra {
                        ui.label(egui::RichText::new(e).font(font).color(egui::Color32::LIGHT_GRAY));
                    }
                });
        });
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
}
