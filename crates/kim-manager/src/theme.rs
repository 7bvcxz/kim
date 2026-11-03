//! Kim design tokens (design/tokens.json) applied to egui.

use eframe::egui;
use std::sync::OnceLock;

const TOKENS: &str = include_str!("../../../design/tokens.json");

#[derive(Debug, Clone, Copy)]
pub struct Colors {
    pub bg_base: egui::Color32,
    pub bg_raised: egui::Color32,
    pub bg_overlay: egui::Color32,
    pub bg_hover: egui::Color32,
    pub bg_selected: egui::Color32,
    pub border_subtle: egui::Color32,
    pub border_strong: egui::Color32,
    pub text_primary: egui::Color32,
    pub text_secondary: egui::Color32,
    pub text_muted: egui::Color32,
    pub text_on_accent: egui::Color32,
    pub accent: egui::Color32,
    pub accent_soft: egui::Color32,
    pub focus: egui::Color32,
    pub success: egui::Color32,
    pub warning: egui::Color32,
    pub danger: egui::Color32,
    pub info: egui::Color32,
}

fn color(tokens: &serde_json::Value, name: &str) -> egui::Color32 {
    let hex = tokens["themes"]["kim-dark"]["color"][name]
        .as_str()
        .unwrap_or("#ff00ff");
    let v = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0xff00ff);
    let [_, r, g, b] = v.to_be_bytes();
    egui::Color32::from_rgb(r, g, b)
}

impl Colors {
    pub fn get() -> Self {
        static COLORS: OnceLock<Colors> = OnceLock::new();
        *COLORS.get_or_init(|| {
            let t: serde_json::Value =
                serde_json::from_str(TOKENS).expect("design/tokens.json is valid");
            Colors {
                bg_base: color(&t, "bg.base"),
                bg_raised: color(&t, "bg.raised"),
                bg_overlay: color(&t, "bg.overlay"),
                bg_hover: color(&t, "bg.hover"),
                bg_selected: color(&t, "bg.selected"),
                border_subtle: color(&t, "border.subtle"),
                border_strong: color(&t, "border.strong"),
                text_primary: color(&t, "text.primary"),
                text_secondary: color(&t, "text.secondary"),
                text_muted: color(&t, "text.muted"),
                text_on_accent: color(&t, "text.on-accent"),
                accent: color(&t, "accent"),
                accent_soft: color(&t, "accent.soft"),
                focus: color(&t, "focus.ring"),
                success: color(&t, "status.success"),
                warning: color(&t, "status.warning"),
                danger: color(&t, "status.danger"),
                info: color(&t, "status.info"),
            }
        })
    }
}

/// Applies the dark theme, compact spacing and the Korean/symbol font fallback chain.
pub fn apply(ctx: &egui::Context) {
    let c = Colors::get();
    let mut v = egui::Visuals::dark();
    v.panel_fill = c.bg_base;
    v.window_fill = c.bg_overlay;
    v.extreme_bg_color = c.bg_base;
    v.faint_bg_color = c.bg_raised;
    v.override_text_color = Some(c.text_primary);
    v.hyperlink_color = c.accent;
    v.selection.bg_fill = c.accent_soft;
    v.selection.stroke = egui::Stroke::new(1.0, c.accent);
    v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, c.border_subtle);
    v.widgets.inactive.bg_fill = c.bg_overlay;
    v.widgets.inactive.weak_bg_fill = c.bg_overlay;
    v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, c.border_strong);
    v.widgets.hovered.bg_fill = c.bg_hover;
    v.widgets.hovered.weak_bg_fill = c.bg_hover;
    v.widgets.active.bg_fill = c.bg_selected;
    v.widgets.active.weak_bg_fill = c.bg_selected;
    v.widgets.hovered.bg_stroke = egui::Stroke::new(2.0, c.focus);
    ctx.set_visuals(v);

    let mut fonts = egui::FontDefinitions::default();
    for (name, file) in [("malgun", "malgun.ttf"), ("symbol", "seguisym.ttf")] {
        if let Ok(bytes) = std::fs::read(format!(r"C:\Windows\Fonts\{file}")) {
            fonts.font_data.insert(
                name.into(),
                std::sync::Arc::new(egui::FontData::from_owned(bytes)),
            );
            for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts.families.entry(fam).or_default().push(name.into());
            }
        }
    }
    ctx.set_fonts(fonts);
}
