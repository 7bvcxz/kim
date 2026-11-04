//! Windows key numbers for the contract's key names (docs/HOTKEY_CONTRACT.md §5.2, §6.1).
//!
//! The engine matches **logical** keys by virtual-key code (Windows assigns them per layout, so
//! `Z` is the key labeled Z on QWERTZ) and **physical** `code:` keys by set-1 scan code (the
//! position, whatever the layout). These numbers are documented Win32 values; they live only in
//! this module and in the compiled trigger tables, and are never stored or printed
//! (`KIM-NFR-022`, `DDALGAK-NFR-005`). The tables are pure data, so they are unit-tested on every
//! platform.

/// Generic and side-specific modifier virtual keys (documented `VK_*` values).
pub const VK_SHIFT: u16 = 0x10;
pub const VK_CONTROL: u16 = 0x11;
pub const VK_MENU: u16 = 0x12;
pub const VK_LSHIFT: u16 = 0xA0;
pub const VK_RSHIFT: u16 = 0xA1;
pub const VK_LCONTROL: u16 = 0xA2;
pub const VK_RCONTROL: u16 = 0xA3;
pub const VK_LMENU: u16 = 0xA4;
pub const VK_RMENU: u16 = 0xA5;
pub const VK_LWIN: u16 = 0x5B;
pub const VK_RWIN: u16 = 0x5C;
pub const VK_ESCAPE: u16 = 0x1B;
pub const VK_CAPITAL: u16 = 0x14;
pub const VK_NUMLOCK: u16 = 0x90;
pub const VK_RETURN: u16 = 0x0D;
/// `KEYEVENTF_UNICODE` input arrives with this virtual key (it is always injected).
pub const VK_PACKET: u16 = 0xE7;
/// `VK_F13`; `VK_F24` is `VK_F13 + 11`.
pub const VK_F13: u16 = 0x7C;

/// Scan code Windows gives the fake left Ctrl that precedes Right Alt on AltGr layouts
/// (`0x21D`: scan `0x1D` with the `0x200` "fake" bit, as low-level hooks report it).
pub const SCAN_ALTGR_FAKE_CTRL: u16 = 0x21D;

/// One of the eight physical modifier keys.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum ModKey {
    LCtrl,
    RCtrl,
    LAlt,
    RAlt,
    LShift,
    RShift,
    LWin,
    RWin,
}

impl ModKey {
    pub const ALL: [ModKey; 8] = [
        ModKey::LCtrl,
        ModKey::RCtrl,
        ModKey::LAlt,
        ModKey::RAlt,
        ModKey::LShift,
        ModKey::RShift,
        ModKey::LWin,
        ModKey::RWin,
    ];

    /// The bit of this key in a [`crate::event::ModState`].
    pub fn bit(self) -> u8 {
        1 << (self as u8)
    }

    pub fn vk(self) -> u16 {
        match self {
            ModKey::LCtrl => VK_LCONTROL,
            ModKey::RCtrl => VK_RCONTROL,
            ModKey::LAlt => VK_LMENU,
            ModKey::RAlt => VK_RMENU,
            ModKey::LShift => VK_LSHIFT,
            ModKey::RShift => VK_RSHIFT,
            ModKey::LWin => VK_LWIN,
            ModKey::RWin => VK_RWIN,
        }
    }

    /// The contract's name of a bare side-specific modifier (`LCtrl`, …).
    pub fn name(self) -> &'static str {
        match self {
            ModKey::LCtrl => "LCtrl",
            ModKey::RCtrl => "RCtrl",
            ModKey::LAlt => "LAlt",
            ModKey::RAlt => "RAlt",
            ModKey::LShift => "LShift",
            ModKey::RShift => "RShift",
            ModKey::LWin => "LWin",
            ModKey::RWin => "RWin",
        }
    }

    pub fn from_name(name: &str) -> Option<ModKey> {
        ModKey::ALL.into_iter().find(|m| m.name() == name)
    }

    /// The modifier a key event belongs to. Low-level hooks report side-specific virtual keys;
    /// injected input may use the generic ones, which are resolved by scan code or the extended
    /// flag (documented: right Ctrl and right Alt are extended keys, right Shift is scan `0x36`).
    pub fn from_event(vk: u16, scan: u16, extended: bool) -> Option<ModKey> {
        Some(match vk {
            VK_LCONTROL => ModKey::LCtrl,
            VK_RCONTROL => ModKey::RCtrl,
            VK_LMENU => ModKey::LAlt,
            VK_RMENU => ModKey::RAlt,
            VK_LSHIFT => ModKey::LShift,
            VK_RSHIFT => ModKey::RShift,
            VK_LWIN => ModKey::LWin,
            VK_RWIN => ModKey::RWin,
            VK_CONTROL if extended => ModKey::RCtrl,
            VK_CONTROL => ModKey::LCtrl,
            VK_MENU if extended => ModKey::RAlt,
            VK_MENU => ModKey::LAlt,
            VK_SHIFT if scan & 0xFF == 0x36 => ModKey::RShift,
            VK_SHIFT => ModKey::LShift,
            _ => return None,
        })
    }
}

/// The virtual key of a logical key name (§5.2), including the IME keys (so they can be
/// recognized and refused) and Ddalgak's lock keys. `NumpadEnter` is `VK_RETURN` with the
/// extended flag (see [`named_needs_extended`]).
pub fn named_vk(name: &str) -> Option<u16> {
    let b = name.as_bytes();
    if b.len() == 1 && (b[0].is_ascii_uppercase() || b[0].is_ascii_digit()) {
        return Some(u16::from(b[0])); // VK_A…VK_Z = 'A'…'Z', VK_0…VK_9 = '0'…'9'
    }
    if let Some(n) = name.strip_prefix('F').and_then(|n| n.parse::<u16>().ok())
        && (1..=24).contains(&n)
    {
        return Some(0x6F + n); // VK_F1 = 0x70
    }
    if let Some(n) = name
        .strip_prefix("Numpad")
        .and_then(|n| n.parse::<u16>().ok())
        && n <= 9
    {
        return Some(0x60 + n); // VK_NUMPAD0
    }
    Some(match name {
        "Escape" => VK_ESCAPE,
        "Tab" => 0x09,
        "Enter" | "NumpadEnter" => VK_RETURN,
        "Space" => 0x20,
        "Backspace" => 0x08,
        "Insert" => 0x2D,
        "Delete" => 0x2E,
        "Home" => 0x24,
        "End" => 0x23,
        "PageUp" => 0x21,
        "PageDown" => 0x22,
        "Up" => 0x26,
        "Down" => 0x28,
        "Left" => 0x25,
        "Right" => 0x27,
        "Plus" => 0xBB,
        "Minus" => 0xBD,
        "Comma" => 0xBC,
        "Period" => 0xBE,
        "NumpadMultiply" => 0x6A,
        "NumpadAdd" => 0x6B,
        "NumpadSubtract" => 0x6D,
        "NumpadDecimal" => 0x6E,
        "NumpadDivide" => 0x6F,
        "PrintScreen" => 0x2C,
        "Pause" => 0x13,
        "ScrollLock" => 0x91,
        "ContextMenu" => 0x5D,
        "MediaNext" => 0xB0,
        "MediaPrevious" => 0xB1,
        "MediaStop" => 0xB2,
        "MediaPlayPause" => 0xB3,
        "VolumeMute" => 0xAD,
        "VolumeDown" => 0xAE,
        "VolumeUp" => 0xAF,
        "HangulMode" | "KanaMode" => 0x15,
        "Hanja" => 0x19,
        "Convert" => 0x1C,
        "NonConvert" => 0x1D,
        "CapsLock" => VK_CAPITAL,
        "NumLock" => VK_NUMLOCK,
        _ => return None,
    })
}

/// Whether a logical name is told apart from another key with the same virtual key by the
/// extended flag (`NumpadEnter` versus `Enter`).
pub fn named_needs_extended(name: &str) -> Option<bool> {
    match name {
        "NumpadEnter" => Some(true),
        "Enter" => Some(false),
        _ => None,
    }
}

/// The logical name of a virtual key, for the few runtime checks that need it (reserved chords
/// under widened matching). `None` for keys without a contract name.
pub fn vk_name(vk: u16, extended: bool) -> Option<&'static str> {
    if vk == VK_RETURN {
        return Some(if extended { "NumpadEnter" } else { "Enter" });
    }
    // Built once, outside any hook callback (the engine calls this from `Table::compile`).
    static NAMES: std::sync::OnceLock<[Option<&'static str>; 256]> = std::sync::OnceLock::new();
    let table = NAMES.get_or_init(|| {
        let mut t = [None; 256];
        for n in kim_hotkeys::keys::NAMED_KEYS
            .iter()
            .chain(["CapsLock", "NumLock"].iter())
        {
            if *n == "NumpadEnter" || *n == "KanaMode" {
                continue; // share a number with Enter / HangulMode
            }
            if let Some(v) = named_vk(n) {
                t[usize::from(v)] = Some(*n);
            }
        }
        t
    });
    table.get(usize::from(vk)).copied().flatten()
}

/// The set-1 scan code (low byte) and extended flag of a physical `code:` key (W3C UI Events
/// `code` values; Windows column of the specification's code table).
pub fn code_scan(code: &str) -> Option<(u16, bool)> {
    let letters: [u16; 26] = [
        0x1E, 0x30, 0x2E, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31, 0x18,
        0x19, 0x10, 0x13, 0x1F, 0x14, 0x16, 0x2F, 0x11, 0x2D, 0x15, 0x2C,
    ];
    if let Some(l) = code.strip_prefix("Key")
        && let [c] = l.as_bytes()
        && c.is_ascii_uppercase()
    {
        return Some((letters[usize::from(c - b'A')], false));
    }
    if let Some(d) = code.strip_prefix("Digit")
        && let [c] = d.as_bytes()
        && c.is_ascii_digit()
    {
        let n = u16::from(c - b'0');
        return Some((if n == 0 { 0x0B } else { 0x01 + n }, false));
    }
    Some(match code {
        "Minus" => (0x0C, false),
        "Equal" => (0x0D, false),
        "BracketLeft" => (0x1A, false),
        "BracketRight" => (0x1B, false),
        "Backslash" => (0x2B, false),
        "Semicolon" => (0x27, false),
        "Quote" => (0x28, false),
        "Backquote" => (0x29, false),
        "Comma" => (0x33, false),
        "Period" => (0x34, false),
        "Slash" => (0x35, false),
        "IntlBackslash" => (0x56, false),
        "IntlRo" => (0x73, false),
        "IntlYen" => (0x7D, false),
        "Lang1" => (0x72, false),
        "Lang2" => (0x71, false),
        "Lang3" => (0x78, false),
        "Lang4" => (0x77, false),
        "Lang5" => (0x76, false),
        _ => return None,
    })
}

/// Virtual keys that are extended keys (their scan code has the `E0` prefix): Right Alt and
/// Right Ctrl, the navigation cluster (Insert, Delete, Home, End, Page Up/Down, arrows), Num Lock,
/// Break, Print Screen, Numpad Divide, the Windows and Application keys, and the browser, media and
/// volume keys (*documented*, "Keyboard Input Overview: extended-key flag"). `MapVirtualKeyW`
/// does not report the prefix for some of them (arrows on this machine), so the list decides.
/// `NumpadEnter` shares `VK_RETURN` with Enter and is marked by its output code instead.
pub fn is_extended_vk(vk: u16) -> bool {
    matches!(
        vk,
        VK_RMENU
            | VK_RCONTROL
            | 0x03 // VK_CANCEL (Break)
            | 0x21..=0x28 // Page Up, Page Down, End, Home, arrows
            | 0x2C..=0x2E // Print Screen, Insert, Delete
            | VK_LWIN
            | VK_RWIN
            | 0x5D // Applications
            | 0x6F // Numpad Divide
            | VK_NUMLOCK
            | 0xA6..=0xB7 // browser, volume, media and launch keys
    )
}

/// Virtual keys that start or switch an input method; never triggers (§7.3).
pub fn is_ime_vk(vk: u16) -> bool {
    matches!(vk, 0x15..=0x19 | 0x1C | 0x1D | 0xE5)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kim_hotkeys::keys::{CODE_KEYS, NAMED_KEYS};

    #[test]
    fn every_contract_key_has_a_number_and_they_are_distinct() {
        for name in NAMED_KEYS {
            assert!(named_vk(name).is_some(), "{name}");
        }
        for code in CODE_KEYS {
            assert!(code_scan(code).is_some(), "code:{code}");
        }
        let mut scans: Vec<u16> = CODE_KEYS.iter().map(|c| code_scan(c).unwrap().0).collect();
        scans.sort_unstable();
        scans.dedup();
        assert_eq!(scans.len(), CODE_KEYS.len(), "scan codes are unique");
        assert_eq!(named_vk("F13"), Some(VK_F13));
        assert_eq!(named_vk("F24"), Some(VK_F13 + 11));
        assert_eq!(named_vk("CapsLock"), Some(VK_CAPITAL));
        assert_eq!(named_vk("Numpad10"), None);
        assert_eq!(code_scan("KeyZ"), Some((0x2C, false)));
        assert_eq!(code_scan("KeyY"), Some((0x15, false)));
        assert_eq!(code_scan("Digit0"), Some((0x0B, false)));
        assert_eq!(code_scan("Digit1"), Some((0x02, false)));
    }

    #[test]
    fn names_round_trip_through_virtual_keys() {
        for name in NAMED_KEYS.iter().chain(["CapsLock", "NumLock"].iter()) {
            if ["KanaMode", "NumpadEnter"].contains(name) {
                continue; // share a number with HangulMode / Enter
            }
            let vk = named_vk(name).unwrap();
            assert_eq!(vk_name(vk, false), Some(*name), "{name}");
        }
        assert_eq!(vk_name(VK_RETURN, true), Some("NumpadEnter"));
        assert_eq!(vk_name(0xFF, false), None);
    }

    #[test]
    fn modifiers_resolve_sides_from_generic_codes() {
        assert_eq!(
            ModKey::from_event(VK_LSHIFT, 0x2A, false),
            Some(ModKey::LShift)
        );
        assert_eq!(
            ModKey::from_event(VK_SHIFT, 0x36, false),
            Some(ModKey::RShift)
        );
        assert_eq!(
            ModKey::from_event(VK_SHIFT, 0x2A, false),
            Some(ModKey::LShift)
        );
        assert_eq!(
            ModKey::from_event(VK_CONTROL, 0x1D, true),
            Some(ModKey::RCtrl)
        );
        assert_eq!(ModKey::from_event(VK_MENU, 0x38, false), Some(ModKey::LAlt));
        assert_eq!(ModKey::from_event(VK_RWIN, 0x5C, true), Some(ModKey::RWin));
        assert_eq!(ModKey::from_event(u16::from(b'A'), 0x1E, false), None);
        let mut bits = 0u8;
        for m in ModKey::ALL {
            assert_eq!(bits & m.bit(), 0);
            bits |= m.bit();
            assert_eq!(ModKey::from_name(m.name()), Some(m));
        }
        assert_eq!(bits, 0xFF);
    }

    #[test]
    fn extended_keys_are_the_documented_ones() {
        for k in [
            "Up",
            "Down",
            "Left",
            "Right",
            "Insert",
            "Delete",
            "Home",
            "End",
            "PageUp",
            "PageDown",
            "PrintScreen",
            "NumLock",
            "NumpadDivide",
            "ContextMenu",
            "MediaNext",
            "VolumeUp",
        ] {
            assert!(is_extended_vk(named_vk(k).unwrap()), "{k}");
        }
        assert!(is_extended_vk(VK_RCONTROL) && is_extended_vk(VK_RMENU));
        for k in [
            "A", "F13", "Escape", "Enter", "Numpad8", "CapsLock", "Space",
        ] {
            assert!(!is_extended_vk(named_vk(k).unwrap()), "{k}");
        }
        assert!(!is_extended_vk(VK_LCONTROL) && !is_extended_vk(VK_RSHIFT));
    }

    #[test]
    fn ime_keys_are_recognized() {
        for k in ["HangulMode", "Hanja", "Convert", "NonConvert"] {
            assert!(is_ime_vk(named_vk(k).unwrap()), "{k}");
        }
        assert!(!is_ime_vk(named_vk("A").unwrap()));
    }
}
