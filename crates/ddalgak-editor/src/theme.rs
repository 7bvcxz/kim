//! The Kim design tokens (design/tokens.json, docs/DESIGN_SYSTEM.md) applied to egui: the
//! near-black `kim-dark` theme, lilac accent for focus and primary actions, 12–13 px text,
//! 28 px controls and the Korean and symbol font fallback from the system fonts (no bundled CJK
//! font, ADR 0014).

use eframe::egui;
use std::sync::OnceLock;

const TOKENS: &str = include_str!("../../../design/tokens.json");

/// The colors the editor uses, by token name.
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

/// Font sizes and control metrics from the tokens.
#[derive(Debug, Clone, Copy)]
pub struct Metrics {
    pub caption: f32,
    pub body: f32,
    pub emphasis: f32,
    pub title: f32,
    pub control_height: f32,
}

fn tokens() -> &'static serde_json::Value {
    static T: OnceLock<serde_json::Value> = OnceLock::new();
    T.get_or_init(|| serde_json::from_str(TOKENS).expect("design/tokens.json is valid"))
}

fn color(name: &str) -> egui::Color32 {
    let hex = tokens()["themes"]["kim-dark"]["color"][name]
        .as_str()
        .unwrap_or("#ff00ff");
    let v = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0xff00ff);
    let [_, r, g, b] = v.to_be_bytes();
    egui::Color32::from_rgb(r, g, b)
}

impl Colors {
    pub fn get() -> Colors {
        static COLORS: OnceLock<Colors> = OnceLock::new();
        *COLORS.get_or_init(|| Colors {
            bg_base: color("bg.base"),
            bg_raised: color("bg.raised"),
            bg_overlay: color("bg.overlay"),
            bg_hover: color("bg.hover"),
            bg_selected: color("bg.selected"),
            border_subtle: color("border.subtle"),
            border_strong: color("border.strong"),
            text_primary: color("text.primary"),
            text_secondary: color("text.secondary"),
            text_muted: color("text.muted"),
            text_on_accent: color("text.on-accent"),
            accent: color("accent"),
            accent_soft: color("accent.soft"),
            focus: color("focus.ring"),
            success: color("status.success"),
            warning: color("status.warning"),
            danger: color("status.danger"),
            info: color("status.info"),
        })
    }
}

impl Metrics {
    pub fn get() -> Metrics {
        let t = tokens();
        let n = |path: &[&str], d: f64| {
            let mut v = t;
            for p in path {
                v = &v[*p];
            }
            v.as_f64().unwrap_or(d) as f32
        };
        Metrics {
            caption: n(&["typography", "size.caption"], 11.0),
            body: n(&["typography", "size.body"], 12.0),
            emphasis: n(&["typography", "size.emphasis"], 13.0),
            title: n(&["typography", "size.title"], 15.0),
            control_height: n(&["size", "control.height"], 28.0),
        }
    }
}

/// Applies the dark theme, the compact spacing and the font fallback chain.
pub fn apply(ctx: &egui::Context) {
    let c = Colors::get();
    let m = Metrics::get();
    let mut v = egui::Visuals::dark();
    v.panel_fill = c.bg_base;
    v.window_fill = c.bg_overlay;
    v.window_stroke = egui::Stroke::new(1.0, c.border_subtle);
    v.extreme_bg_color = c.bg_raised;
    v.faint_bg_color = c.bg_raised;
    v.override_text_color = Some(c.text_primary);
    v.hyperlink_color = c.accent;
    v.selection.bg_fill = c.accent_soft;
    v.selection.stroke = egui::Stroke::new(1.0, c.accent);
    v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, c.border_subtle);
    v.widgets.noninteractive.bg_fill = c.bg_raised;
    v.widgets.inactive.bg_fill = c.bg_overlay;
    v.widgets.inactive.weak_bg_fill = c.bg_overlay;
    v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, c.border_strong);
    v.widgets.hovered.bg_fill = c.bg_hover;
    v.widgets.hovered.weak_bg_fill = c.bg_hover;
    v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, c.focus);
    v.widgets.active.bg_fill = c.bg_selected;
    v.widgets.active.weak_bg_fill = c.bg_selected;
    // The keyboard focus ring (2 px, focus.ring): every focused control shows it.
    v.widgets.open.bg_stroke = egui::Stroke::new(2.0, c.focus);
    // The editor is always dark (the design system's kim-dark theme), whatever the OS says.
    ctx.set_theme(egui::Theme::Dark);
    ctx.set_visuals(v);

    ctx.all_styles_mut(|s| {
        use egui::{FontId, TextStyle};
        s.text_styles = [
            (TextStyle::Small, FontId::proportional(m.caption)),
            (TextStyle::Body, FontId::proportional(m.body)),
            (TextStyle::Button, FontId::proportional(m.body)),
            (TextStyle::Heading, FontId::proportional(m.title)),
            (TextStyle::Monospace, FontId::monospace(m.body)),
        ]
        .into();
        s.spacing.item_spacing = egui::vec2(6.0, 6.0);
        s.spacing.button_padding = egui::vec2(8.0, 4.0);
        s.spacing.interact_size.y = m.control_height - 4.0;
    });

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_theme_is_the_near_black_kim_dark_tokens() {
        let c = Colors::get();
        assert_eq!(c.bg_base, egui::Color32::from_rgb(0x0E, 0x0E, 0x12));
        assert_eq!(c.accent, egui::Color32::from_rgb(0xB7, 0xA6, 0xF5));
        let m = Metrics::get();
        assert!((m.body - 12.0).abs() < f32::EPSILON);
        assert!((m.emphasis - 13.0).abs() < f32::EPSILON);
        assert!((m.control_height - 28.0).abs() < f32::EPSILON);
    }
}
