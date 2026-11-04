//! Terminal colour themes: the default foreground and background, the cursor, the selection and
//! the 16 ANSI colours. Indexed colours 16–255 follow xterm's cube and grey ramp; 24-bit colours
//! are drawn exactly (docs/DESIGN_SYSTEM.md §2.2).

/// A colour in sRGB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// `#RRGGBB`.
    pub fn hex(s: &str) -> Option<Self> {
        let h = s.strip_prefix('#')?;
        if h.len() != 6 {
            return None;
        }
        let v = u32::from_str_radix(h, 16).ok()?;
        Some(Self((v >> 16) as u8, (v >> 8) as u8, v as u8))
    }

    /// `0x00RRGGBB`, the frame's pixel format.
    pub fn to_u32(self) -> u32 {
        (u32::from(self.0) << 16) | (u32::from(self.1) << 8) | u32::from(self.2)
    }

    pub fn from_u32(v: u32) -> Self {
        Self((v >> 16) as u8, (v >> 8) as u8, v as u8)
    }

    /// This colour moved toward `other` by `t`/255.
    pub fn mix(self, other: Self, t: u8) -> Self {
        let m = |a: u8, b: u8| {
            let (a, b, t) = (i32::from(a), i32::from(b), i32::from(t));
            u8::try_from(a + (b - a) * t / 255).unwrap_or(0)
        };
        Self(m(self.0, other.0), m(self.1, other.1), m(self.2, other.2))
    }
}

/// The colours a terminal view is drawn with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    pub name: String,
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
    pub selection: Rgb,
    /// The background of search matches (Step 19), and of the current match with its text
    /// colour (a light fill under dark text, so it differs from the others in lightness, not
    /// only in hue).
    pub search_match: Rgb,
    pub search_current: Rgb,
    pub search_current_text: Rgb,
    /// ANSI 0–7 and their bright variants 8–15.
    pub ansi: [Rgb; 16],
}

/// The `kim-dark` search tokens (`terminal.search-match`, `terminal.search-current`; checked
/// against design/tokens.json by a test).
const KIM_DARK_SEARCH: [&str; 2] = ["#4D3F1C", "#E8C27A"];

/// The `kim-dark` terminal tokens in `design/tokens.json` order (checked against the file by a
/// test): foreground, background, cursor, selection, then black … bright-white.
const KIM_DARK: [&str; 20] = [
    "#D8D6E0", "#0E0E12", "#B7A6F5", "#3A3460", "#3A3848", "#F0868F", "#8BC79A", "#E8C27A",
    "#8AB4F8", "#C9A6F5", "#7FD3D9", "#D8D6E0", "#8E8AA3", "#FFA3AB", "#A8E0B6", "#F5D79C",
    "#A9C8FF", "#DCC4FF", "#A2E6EA", "#F5F4FA",
];

impl Theme {
    /// The default theme (`kim-dark`, docs/DESIGN_SYSTEM.md §2.2).
    pub fn kim_dark() -> Self {
        let c = |i: usize| Rgb::hex(KIM_DARK[i]).unwrap_or(Rgb(0, 0, 0));
        let mut ansi = [Rgb(0, 0, 0); 16];
        for (i, a) in ansi.iter_mut().enumerate() {
            *a = c(4 + i);
        }
        Self {
            name: "kim-dark".into(),
            foreground: c(0),
            background: c(1),
            cursor: c(2),
            selection: c(3),
            search_match: Rgb::hex(KIM_DARK_SEARCH[0]).unwrap_or(Rgb(0, 0, 0)),
            search_current: Rgb::hex(KIM_DARK_SEARCH[1]).unwrap_or(Rgb(0, 0, 0)),
            search_current_text: c(1),
            ansi,
        }
    }

    /// A Windows contrast theme (docs/DESIGN_SYSTEM.md §4.3): the system's window, text and
    /// highlight colours, no lilac accent. ANSI colours keep their hues but are pushed toward the
    /// text colour so they stay readable on the system background.
    pub fn high_contrast(window: Rgb, text: Rgb, highlight: Rgb) -> Self {
        let base = Self::kim_dark();
        let mut ansi = base.ansi;
        for a in &mut ansi {
            *a = a.mix(text, 96);
        }
        ansi[0] = window.mix(text, 64);
        ansi[7] = text;
        ansi[15] = text;
        Self {
            name: "high-contrast".into(),
            foreground: text,
            background: window,
            cursor: text,
            selection: highlight,
            // Matches in the highlight colour; the current one in reverse video.
            search_match: highlight,
            search_current: text,
            search_current_text: window,
            ansi,
        }
    }

    /// The colour of palette index `n`: the theme's 16 colours, then xterm's 6×6×6 cube and its
    /// 24-step grey ramp.
    pub fn indexed(&self, n: u8) -> Rgb {
        match n {
            0..=15 => self.ansi[usize::from(n)],
            16..=231 => {
                let i = n - 16;
                let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                Rgb(level(i / 36), level((i / 6) % 6), level(i % 6))
            }
            _ => {
                let v = 8 + (n - 232) * 10;
                Rgb(v, v, v)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kim_dark_matches_the_design_tokens() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../design/tokens.json");
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let colors = &doc["themes"]["kim-dark"]["color"];
        let get = |name: &str| -> Rgb {
            let v = find(colors, name).unwrap_or_else(|| {
                panic!("token {name} not found in tokens.json themes.kim-dark.color");
            });
            Rgb::hex(&v).unwrap()
        };
        let t = Theme::kim_dark();
        assert_eq!(t.foreground, get("terminal.foreground"));
        assert_eq!(t.background, get("terminal.background"));
        assert_eq!(t.cursor, get("terminal.cursor"));
        assert_eq!(t.selection, get("terminal.selection"));
        assert_eq!(t.search_match, get("terminal.search-match"));
        assert_eq!(t.search_current, get("terminal.search-current"));
        assert_eq!(t.search_current_text, get("terminal.background"));
        let names = [
            "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
        ];
        for (i, n) in names.iter().enumerate() {
            assert_eq!(t.ansi[i], get(&format!("ansi.{n}")), "ansi.{n}");
            assert_eq!(
                t.ansi[i + 8],
                get(&format!("ansi.bright-{n}")),
                "bright {n}"
            );
        }
    }

    /// Finds a token by name anywhere in the token document.
    fn find(v: &serde_json::Value, name: &str) -> Option<String> {
        match v {
            serde_json::Value::Object(m) => {
                if let Some(serde_json::Value::String(s)) = m.get(name) {
                    return Some(s.clone());
                }
                m.values().find_map(|c| find(c, name))
            }
            serde_json::Value::Array(a) => a.iter().find_map(|c| find(c, name)),
            _ => None,
        }
    }

    #[test]
    fn indexed_colours_follow_xterm() {
        let t = Theme::kim_dark();
        assert_eq!(t.indexed(1), t.ansi[1]);
        assert_eq!(t.indexed(16), Rgb(0, 0, 0));
        assert_eq!(t.indexed(196), Rgb(255, 0, 0));
        assert_eq!(t.indexed(208), Rgb(255, 135, 0));
        assert_eq!(t.indexed(231), Rgb(255, 255, 255));
        assert_eq!(t.indexed(232), Rgb(8, 8, 8));
        assert_eq!(t.indexed(255), Rgb(238, 238, 238));
        assert_eq!(Rgb::hex("#0E0E12"), Some(Rgb(14, 14, 18)));
        assert_eq!(
            Rgb(0, 0, 0).mix(Rgb(255, 255, 255), 255),
            Rgb(255, 255, 255)
        );
    }
}
