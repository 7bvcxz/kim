//! The colour picker's values as text (Step 59; NOEUL-FR-044, NOEUL-FR-092, ADR 0053).
//!
//! `color.copy` (`C`) in the capture overlay copies the colour of the frozen picture's pixel under
//! the magnifier's crosshair as text in one of three formats, and `color.format-next`
//! (`Shift+C`) switches between them ([`Format::next`]):
//!
//! | Format | Opaque | With alpha (`[color] alpha = "on"`) |
//! | --- | --- | --- |
//! | HEX | `#3A6EA5` (`[color] hex_case = "lower"`: `#3a6ea5`) | `#3A6EA5FF` |
//! | RGB | `rgb(58, 110, 165)` | `rgba(58, 110, 165, 1)` |
//! | HSL | `hsl(211, 48%, 44%)` | `hsla(211, 48%, 44%, 1)` |
//!
//! Every string is CSS, so it pastes into style sheets and design tools as it is, and `F3`
//! ([`crate::paste::parse_color`]) turns each of them back into a colour swatch. HEX and RGB are
//! exact; HSL is rounded to whole degrees and percent, so a colour taken back from its HSL text can
//! differ from the pixel by a few units per channel ([`from_hsl`]). The alpha is a number 0–1 with
//! at most three decimals, enough to give every 8-bit alpha back exactly. A screen picture is
//! opaque, so its alpha is always `FF` / `1`; the option exists for tools that want eight-digit
//! HEX or `rgba()`.
//!
//! The values are those of the captured picture: what Windows' GDI capture gives for the desktop,
//! 8-bit sRGB values before the display's colour profile is applied, and on an HDR monitor the SDR
//! picture Windows converted it to. They can differ from what the display shows (NOEUL-FR-092).

use crate::paste::Rgba;

/// A text format of a colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Hex,
    Rgb,
    Hsl,
}

impl Format {
    pub const ALL: [Format; 3] = [Format::Hex, Format::Rgb, Format::Hsl];

    pub fn as_str(self) -> &'static str {
        match self {
            Format::Hex => "hex",
            Format::Rgb => "rgb",
            Format::Hsl => "hsl",
        }
    }

    pub fn parse(s: &str) -> Option<Format> {
        Format::ALL.into_iter().find(|f| f.as_str() == s)
    }

    /// The name people read: `HEX`, `RGB`, `HSL`.
    pub fn title(self) -> &'static str {
        match self {
            Format::Hex => "HEX",
            Format::Rgb => "RGB",
            Format::Hsl => "HSL",
        }
    }

    /// The next format of `color.format-next`: HEX → RGB → HSL → HEX.
    #[must_use]
    pub fn next(self) -> Format {
        match self {
            Format::Hex => Format::Rgb,
            Format::Rgb => Format::Hsl,
            Format::Hsl => Format::Hex,
        }
    }
}

/// Upper- or lowercase HEX digits (`[color] hex_case`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HexCase {
    #[default]
    Upper,
    Lower,
}

impl HexCase {
    pub fn as_str(self) -> &'static str {
        match self {
            HexCase::Upper => "upper",
            HexCase::Lower => "lower",
        }
    }

    pub fn parse(s: &str) -> Option<HexCase> {
        [HexCase::Upper, HexCase::Lower]
            .into_iter()
            .find(|c| c.as_str() == s)
    }
}

/// How the picker writes a colour: the chosen format (what `C` copies), the HEX case and whether
/// the alpha is included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    pub format: Format,
    pub case: HexCase,
    pub alpha: bool,
}

impl Default for Style {
    fn default() -> Style {
        Style {
            format: Format::Hex,
            case: HexCase::Upper,
            alpha: false,
        }
    }
}

impl Style {
    /// `c` in the chosen format: exactly the text `color.copy` puts on the clipboard.
    pub fn text(&self, c: Rgba) -> String {
        text(c, self.format, self)
    }

    /// The same style with another chosen format.
    #[must_use]
    pub fn with_format(mut self, format: Format) -> Style {
        self.format = format;
        self
    }
}

/// `c` as text in `format`, with the HEX case and alpha of `style` (its chosen format is not
/// used, so the magnifier writes every line with the same options).
pub fn text(c: Rgba, format: Format, style: &Style) -> String {
    match format {
        Format::Hex => {
            let s = if style.alpha {
                format!("#{:02X}{:02X}{:02X}{:02X}", c.r, c.g, c.b, c.a)
            } else {
                format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b)
            };
            match style.case {
                HexCase::Upper => s,
                HexCase::Lower => s.to_ascii_lowercase(),
            }
        }
        Format::Rgb => {
            if style.alpha {
                format!("rgba({}, {}, {}, {})", c.r, c.g, c.b, alpha_text(c.a))
            } else {
                format!("rgb({}, {}, {})", c.r, c.g, c.b)
            }
        }
        Format::Hsl => {
            let (h, s, l) = hsl(c);
            if style.alpha {
                format!("hsla({h}, {s}%, {l}%, {})", alpha_text(c.a))
            } else {
                format!("hsl({h}, {s}%, {l}%)")
            }
        }
    }
}

/// The widest text of `format` with these options (the monospaced readout is sized with it
/// before the pixel is known).
pub fn widest(format: Format, style: &Style) -> String {
    match (format, style.alpha) {
        (Format::Hex, false) => "#FFFFFF".into(),
        (Format::Hex, true) => "#FFFFFFFF".into(),
        (Format::Rgb, false) => "rgb(255, 255, 255)".into(),
        (Format::Rgb, true) => "rgba(255, 255, 255, 0.502)".into(),
        (Format::Hsl, false) => "hsl(359, 100%, 100%)".into(),
        (Format::Hsl, true) => "hsla(359, 100%, 100%, 0.502)".into(),
    }
}

/// An 8-bit alpha as a number 0–1: at most three decimals, trailing zeros dropped (`255` → `1`,
/// `128` → `0.502`, `0` → `0`). Three decimals give every alpha back exactly (×255, rounded).
pub fn alpha_text(a: u8) -> String {
    let milli = (f64::from(a) * 1000.0 / 255.0).round() as u32;
    if milli.is_multiple_of(1000) {
        return (milli / 1000).to_string();
    }
    let s = format!("{}.{:03}", milli / 1000, milli % 1000);
    s.trim_end_matches('0').to_string()
}

/// The HSL of an 8-bit colour: hue in whole degrees 0–359, saturation and lightness in whole
/// percent 0–100 (rounded half away from zero; a grey has hue 0 and saturation 0).
pub fn hsl(c: Rgba) -> (u16, u8, u8) {
    let r = f64::from(c.r) / 255.0;
    let g = f64::from(c.g) / 255.0;
    let b = f64::from(c.b) / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    let (h, s) = if d == 0.0 {
        (0.0, 0.0)
    } else {
        let s = d / (1.0 - (2.0 * l - 1.0).abs());
        let h = if max == r {
            60.0 * ((g - b) / d).rem_euclid(6.0)
        } else if max == g {
            60.0 * ((b - r) / d + 2.0)
        } else {
            60.0 * ((r - g) / d + 4.0)
        };
        (h, s)
    };
    let h = (h.round() as u16) % 360;
    let pct = |v: f64| (v * 100.0).round().clamp(0.0, 100.0) as u8;
    (h, pct(s), pct(l))
}

/// The 8-bit colour of hue `h` (degrees, any value: taken modulo 360), saturation `s` and
/// lightness `l` (0–1), rounded to the nearest unit.
pub fn from_hsl(h: f64, s: f64, l: f64) -> (u8, u8, u8) {
    let h = h.rem_euclid(360.0);
    let s = s.clamp(0.0, 1.0);
    let l = l.clamp(0.0, 1.0);
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h / 60.0;
    let x = c * (1.0 - (hp.rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let v = |t: f64| ((t + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    (v(r), v(g), v(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paste::parse_color;

    const STEEL: Rgba = Rgba::rgb(0x3A, 0x6E, 0xA5);

    fn style(format: Format, case: HexCase, alpha: bool) -> Style {
        Style {
            format,
            case,
            alpha,
        }
    }

    /// The exact text `C` copies in every format and option (NOEUL-FR-044; goldens).
    #[test]
    fn every_format_is_written_exactly() {
        let cases: &[(Format, HexCase, bool, Rgba, &str)] = &[
            (Format::Hex, HexCase::Upper, false, STEEL, "#3A6EA5"),
            (Format::Hex, HexCase::Lower, false, STEEL, "#3a6ea5"),
            (Format::Hex, HexCase::Upper, true, STEEL, "#3A6EA5FF"),
            (Format::Hex, HexCase::Lower, true, STEEL, "#3a6ea5ff"),
            (
                Format::Rgb,
                HexCase::Upper,
                false,
                STEEL,
                "rgb(58, 110, 165)",
            ),
            (
                Format::Rgb,
                HexCase::Lower,
                true,
                STEEL,
                "rgba(58, 110, 165, 1)",
            ),
            (
                Format::Hsl,
                HexCase::Upper,
                false,
                STEEL,
                "hsl(211, 48%, 44%)",
            ),
            (
                Format::Hsl,
                HexCase::Upper,
                true,
                STEEL,
                "hsla(211, 48%, 44%, 1)",
            ),
            (
                Format::Hex,
                HexCase::Upper,
                false,
                Rgba::rgb(0, 0, 0),
                "#000000",
            ),
            (
                Format::Rgb,
                HexCase::Upper,
                false,
                Rgba::rgb(255, 255, 255),
                "rgb(255, 255, 255)",
            ),
            (
                Format::Hsl,
                HexCase::Upper,
                false,
                Rgba::rgb(255, 255, 255),
                "hsl(0, 0%, 100%)",
            ),
            (
                Format::Hsl,
                HexCase::Upper,
                false,
                Rgba::rgb(128, 128, 128),
                "hsl(0, 0%, 50%)",
            ),
            (
                Format::Hsl,
                HexCase::Upper,
                false,
                Rgba::rgb(255, 0, 0),
                "hsl(0, 100%, 50%)",
            ),
            (
                Format::Hsl,
                HexCase::Upper,
                false,
                Rgba::rgb(0, 255, 0),
                "hsl(120, 100%, 50%)",
            ),
            (
                Format::Hsl,
                HexCase::Upper,
                false,
                Rgba::rgb(0, 0, 255),
                "hsl(240, 100%, 50%)",
            ),
            (
                Format::Hsl,
                HexCase::Upper,
                false,
                Rgba::rgb(30, 144, 255),
                "hsl(210, 100%, 56%)",
            ),
            (
                Format::Hsl,
                HexCase::Upper,
                false,
                Rgba::rgb(255, 0, 1),
                "hsl(0, 100%, 50%)",
            ),
            (
                Format::Rgb,
                HexCase::Upper,
                true,
                Rgba { a: 128, ..STEEL },
                "rgba(58, 110, 165, 0.502)",
            ),
            (
                Format::Hex,
                HexCase::Upper,
                true,
                Rgba { a: 0, ..STEEL },
                "#3A6EA500",
            ),
        ];
        for (format, case, alpha, c, want) in cases {
            let got = text(*c, *format, &style(*format, *case, *alpha));
            assert_eq!(got, *want, "{format:?} {case:?} alpha {alpha} of {c:?}");
            assert_eq!(style(*format, *case, *alpha).text(*c), *want);
        }
    }

    #[test]
    fn the_format_cycles_hex_rgb_hsl_and_names_round_trip() {
        assert_eq!(Format::Hex.next(), Format::Rgb);
        assert_eq!(Format::Rgb.next(), Format::Hsl);
        assert_eq!(Format::Hsl.next(), Format::Hex);
        for f in Format::ALL {
            assert_eq!(Format::parse(f.as_str()), Some(f));
        }
        assert_eq!(Format::parse("HEX"), None, "names are lowercase");
        for c in [HexCase::Upper, HexCase::Lower] {
            assert_eq!(HexCase::parse(c.as_str()), Some(c));
        }
        assert_eq!(Style::default().format, Format::Hex);
        assert_eq!(
            Style::default().with_format(Format::Hsl).format,
            Format::Hsl
        );
    }

    /// Every alpha comes back exactly from its text, and the text is at most three decimals.
    #[test]
    fn alpha_text_gives_every_alpha_back() {
        assert_eq!(alpha_text(255), "1");
        assert_eq!(alpha_text(0), "0");
        assert_eq!(alpha_text(128), "0.502");
        assert_eq!(alpha_text(51), "0.2");
        for a in 0..=255u8 {
            let t = alpha_text(a);
            assert!(t.len() <= 5, "{t}");
            let back = (t.parse::<f64>().unwrap() * 255.0).round() as u8;
            assert_eq!(back, a, "{t}");
        }
    }

    /// The widest strings are at least as long as any value (the readout never clips).
    #[test]
    fn the_widest_text_is_never_shorter_than_a_value() {
        let samples = [
            Rgba::rgb(255, 255, 255),
            Rgba::rgb(0, 0, 0),
            Rgba::rgb(255, 0, 1),
            Rgba {
                a: 1,
                ..Rgba::rgb(200, 100, 50)
            },
            STEEL,
        ];
        for f in Format::ALL {
            for alpha in [false, true] {
                let st = style(f, HexCase::Upper, alpha);
                let w = widest(f, &st).chars().count();
                for c in samples {
                    assert!(text(c, f, &st).chars().count() <= w, "{f:?} {c:?}");
                }
            }
        }
    }

    /// What `C` copies is what `F3` pins (NOEUL-FR-071): HEX and RGB come back exactly for every
    /// option, HSL within the rounding of whole degrees and percent.
    #[test]
    fn every_copied_text_pastes_back_as_the_same_colour() {
        let mut worst = 0u8;
        for r in (0..=255u8).step_by(15) {
            for g in (0..=255u8).step_by(17) {
                for b in (0..=255u8).step_by(51) {
                    for a in [255u8, 128, 0] {
                        let c = Rgba { r, g, b, a };
                        for case in [HexCase::Upper, HexCase::Lower] {
                            for alpha in [false, true] {
                                let st = style(Format::Hex, case, alpha);
                                let want = if alpha { c } else { Rgba { a: 255, ..c } };
                                for f in [Format::Hex, Format::Rgb] {
                                    let t = text(c, f, &st);
                                    assert_eq!(parse_color(&t), Some(want), "{t}");
                                }
                                let t = text(c, Format::Hsl, &st);
                                let back = parse_color(&t).unwrap_or_else(|| panic!("{t}"));
                                assert_eq!(back.a, want.a, "{t}");
                                for (x, y) in [(back.r, c.r), (back.g, c.g), (back.b, c.b)] {
                                    worst = worst.max(x.abs_diff(y));
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(worst <= 3, "HSL rounding stays within 3 units: {worst}");
    }

    /// HSL and back over the whole cube: never more than 3 units per channel apart.
    #[test]
    fn hsl_conversions_agree_with_known_values() {
        assert_eq!(hsl(STEEL), (211, 48, 44));
        assert_eq!(from_hsl(0.0, 1.0, 0.5), (255, 0, 0));
        assert_eq!(from_hsl(120.0, 1.0, 0.5), (0, 255, 0));
        assert_eq!(from_hsl(240.0, 1.0, 0.5), (0, 0, 255));
        assert_eq!(from_hsl(360.0, 1.0, 0.5), (255, 0, 0), "360° is 0°");
        assert_eq!(from_hsl(-120.0, 1.0, 0.5), (0, 0, 255));
        assert_eq!(from_hsl(0.0, 0.0, 1.0), (255, 255, 255));
        assert_eq!(from_hsl(0.0, 0.0, 0.0), (0, 0, 0));
        assert_eq!(from_hsl(39.0, 1.0, 0.5), (255, 166, 0));
        for r in (0..=255u8).step_by(5) {
            for g in (0..=255u8).step_by(5) {
                for b in (0..=255u8).step_by(5) {
                    let (h, s, l) = hsl(Rgba::rgb(r, g, b));
                    assert!(h < 360 && s <= 100 && l <= 100);
                    let (r2, g2, b2) =
                        from_hsl(f64::from(h), f64::from(s) / 100.0, f64::from(l) / 100.0);
                    for (x, y) in [(r, r2), (g, g2), (b, b2)] {
                        assert!(
                            x.abs_diff(y) <= 3,
                            "{r},{g},{b} → {h},{s},{l} → {r2},{g2},{b2}"
                        );
                    }
                }
            }
        }
    }
}
