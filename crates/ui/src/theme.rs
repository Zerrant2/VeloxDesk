//! Theme-aware semantic color palette for both Dark and Light egui themes.
//!
//! All status text, banner backgrounds, strokes, and floating overlays select colors from
//! [`Palette`] based on `ui.visuals().dark_mode` so every screen maintains high readability
//! in both Dark and Light modes.

/// Semantic colors calibrated for readability on both Dark and Light egui surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// Positive / ready / active / accept text and indicator dots.
    pub success: egui::Color32,
    /// Informational / active-session / update-available accent text and indicator dots.
    pub info: egui::Color32,
    /// Warning / fallback notice text.
    pub warning: egui::Color32,
    /// Error / destructive / stop / reject / disconnect text.
    pub danger: egui::Color32,
    /// Offline / neutral / unset status text and indicator dots.
    pub muted: egui::Color32,
    /// Info banner background fill, border stroke, and heading text.
    pub info_banner_bg: egui::Color32,
    pub info_banner_stroke: egui::Color32,
    pub info_banner_text: egui::Color32,
    /// Error banner background fill, border stroke, and body text.
    pub error_banner_bg: egui::Color32,
    pub error_banner_stroke: egui::Color32,
    pub error_banner_text: egui::Color32,
    /// Floating session toolbar background fill and border stroke.
    pub toolbar_bg: egui::Color32,
    pub toolbar_stroke: egui::Color32,
    /// Active toggle pill (e.g. OTP `CAPS` badge) fill and label text.
    pub badge_active_bg: egui::Color32,
    pub badge_active_text: egui::Color32,
    /// Subtle frame border around dark terminal output viewport.
    pub terminal_stroke: egui::Color32,
}

impl Palette {
    /// High-contrast semantic palette for Dark mode.
    pub fn dark() -> Self {
        Self {
            success: egui::Color32::from_rgb(122, 232, 148),
            info: egui::Color32::from_rgb(120, 204, 255),
            warning: egui::Color32::from_rgb(245, 200, 110),
            danger: egui::Color32::from_rgb(255, 126, 126),
            muted: egui::Color32::from_rgb(158, 162, 172),
            info_banner_bg: egui::Color32::from_rgb(28, 52, 74),
            info_banner_stroke: egui::Color32::from_rgb(80, 155, 220),
            info_banner_text: egui::Color32::from_rgb(190, 232, 255),
            error_banner_bg: egui::Color32::from_rgb(68, 28, 28),
            error_banner_stroke: egui::Color32::from_rgb(210, 95, 95),
            error_banner_text: egui::Color32::from_rgb(255, 165, 165),
            toolbar_bg: egui::Color32::from_rgba_unmultiplied(18, 20, 24, 215),
            toolbar_stroke: egui::Color32::from_rgba_unmultiplied(255, 255, 255, 32),
            badge_active_bg: egui::Color32::from_rgb(36, 112, 192),
            badge_active_text: egui::Color32::WHITE,
            terminal_stroke: egui::Color32::from_rgb(56, 62, 72),
        }
    }

    /// High-contrast semantic palette for Light mode (WCAG AA >= 4.5:1 on light surfaces).
    pub fn light() -> Self {
        Self {
            success: egui::Color32::from_rgb(18, 118, 48),
            info: egui::Color32::from_rgb(14, 92, 174),
            warning: egui::Color32::from_rgb(148, 86, 0),
            danger: egui::Color32::from_rgb(182, 28, 28),
            muted: egui::Color32::from_rgb(92, 98, 108),
            info_banner_bg: egui::Color32::from_rgb(228, 241, 255),
            info_banner_stroke: egui::Color32::from_rgb(72, 138, 206),
            info_banner_text: egui::Color32::from_rgb(14, 66, 124),
            error_banner_bg: egui::Color32::from_rgb(254, 232, 232),
            error_banner_stroke: egui::Color32::from_rgb(206, 76, 76),
            error_banner_text: egui::Color32::from_rgb(155, 24, 24),
            toolbar_bg: egui::Color32::from_rgba_unmultiplied(252, 253, 255, 246),
            toolbar_stroke: egui::Color32::from_rgb(188, 195, 206),
            badge_active_bg: egui::Color32::from_rgb(20, 100, 184),
            badge_active_text: egui::Color32::WHITE,
            terminal_stroke: egui::Color32::from_rgb(160, 166, 176),
        }
    }
}

/// Return the semantic [`Palette`] matching `dark_mode`.
pub fn palette(dark_mode: bool) -> Palette {
    if dark_mode { Palette::dark() } else { Palette::light() }
}

/// Return the semantic [`Palette`] for the active egui theme of `ui`.
pub fn palette_for(ui: &egui::Ui) -> Palette {
    palette(ui.visuals().dark_mode)
}

/// Floating top-edge toolbar frame styled for the active Dark or Light theme.
pub fn toolbar_frame(ui: &egui::Ui) -> egui::Frame {
    let p = palette_for(ui);
    egui::Frame::new()
        .fill(p.toolbar_bg)
        .stroke(egui::Stroke::new(1.0, p.toolbar_stroke))
        .corner_radius(egui::CornerRadius { nw: 0, ne: 0, sw: 6, se: 6 })
        .inner_margin(egui::Margin::symmetric(8, 3))
}

/// Floating pill overlay frame (e.g. "Waiting for video…") readable over the black video canvas
/// in both Dark and Light modes.
pub fn overlay_frame(ui: &egui::Ui) -> egui::Frame {
    let p = palette_for(ui);
    egui::Frame::new()
        .fill(p.toolbar_bg)
        .stroke(egui::Stroke::new(1.0, p.toolbar_stroke))
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(12, 8))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn srgb_channel(c: u8) -> f64 {
        let v = c as f64 / 255.0;
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }

    fn relative_luminance(c: egui::Color32) -> f64 {
        0.2126 * srgb_channel(c.r()) + 0.7152 * srgb_channel(c.g()) + 0.0722 * srgb_channel(c.b())
    }

    fn contrast_ratio(a: egui::Color32, b: egui::Color32) -> f64 {
        let l1 = relative_luminance(a);
        let l2 = relative_luminance(b);
        let (hi, lo) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
        (hi + 0.05) / (lo + 0.05)
    }

    #[test]
    fn dark_and_light_palettes_meet_wcag_aa_contrast() {
        let dark = Palette::dark();
        let dark_bg = egui::Visuals::dark().panel_fill;
        let dark_btn = egui::Visuals::dark().widgets.inactive.weak_bg_fill;
        for (name, fg) in [
            ("dark.success", dark.success),
            ("dark.info", dark.info),
            ("dark.warning", dark.warning),
            ("dark.danger", dark.danger),
            ("dark.muted", dark.muted),
        ] {
            let r_panel = contrast_ratio(fg, dark_bg);
            let r_btn = contrast_ratio(fg, dark_btn);
            assert!(r_panel >= 4.5, "{name} panel contrast {r_panel:.2} < 4.5");
            assert!(r_btn >= 4.5, "{name} button contrast {r_btn:.2} < 4.5");
        }
        assert!(contrast_ratio(dark.info_banner_text, dark.info_banner_bg) >= 7.0);
        assert!(contrast_ratio(dark.error_banner_text, dark.error_banner_bg) >= 4.5);
        assert!(contrast_ratio(dark.badge_active_text, dark.badge_active_bg) >= 4.5);

        let light = Palette::light();
        let light_bg = egui::Visuals::light().panel_fill;
        let light_btn = egui::Visuals::light().widgets.inactive.weak_bg_fill;
        for (name, fg) in [
            ("light.success", light.success),
            ("light.info", light.info),
            ("light.warning", light.warning),
            ("light.danger", light.danger),
            ("light.muted", light.muted),
        ] {
            let r_panel = contrast_ratio(fg, light_bg);
            let r_btn = contrast_ratio(fg, light_btn);
            assert!(r_panel >= 4.5, "{name} panel contrast {r_panel:.2} < 4.5");
            assert!(r_btn >= 4.5, "{name} button contrast {r_btn:.2} < 4.5");
        }
        assert!(contrast_ratio(light.info_banner_text, light.info_banner_bg) >= 7.0);
        assert!(contrast_ratio(light.error_banner_text, light.error_banner_bg) >= 4.5);
        assert!(contrast_ratio(light.badge_active_text, light.badge_active_bg) >= 4.5);

        // Toolbar text in each mode must meet WCAG AA (>= 4.5:1) against toolbar_bg.
        let light_text = egui::Visuals::light().text_color();
        let dark_text = egui::Visuals::dark().text_color();
        assert!(contrast_ratio(light_text, light.toolbar_bg) >= 7.0);
        assert!(contrast_ratio(dark_text, dark.toolbar_bg) >= 4.5);
    }
}
