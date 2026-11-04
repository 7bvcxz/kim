//! Keyboard dispatch of the window (ADR 0009 order, docs/SUBAK_GUI.md §3): the IME guard, then
//! the window's own shortcuts, then everything else to the pane in the xterm encoding of
//! `subak-term`.
//!
//! The shortcuts are written in the Kim hotkey contract's key syntax (docs/HOTKEY_CONTRACT.md §5)
//! and parsed and mapped to virtual keys by `kim-hotkeys`, in the context `terminal` (keys the
//! window does not bind go to the program). They are Windows Terminal's defaults for the same
//! actions. Step 17 adds them to Subak's hotkey manifest, so they can be rebound with
//! `subak hotkeys`; until then they are fixed. The `Ctrl+B` prefix is Step 23's: Step 16 sends
//! `Ctrl+B` to the program.

use kim_hotkeys::keys::Chord;
use kim_hotkeys::register::win32_chord;
use subak_term::{Key, Mods};

/// A window action a shortcut runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shortcut {
    Copy,
    Paste,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    ScrollPageUp,
    ScrollPageDown,
    ScrollLineUp,
    ScrollLineDown,
    ScrollTop,
    ScrollBottom,
}

/// One action: its stable id (the contract's action-id syntax), title and default keys.
#[derive(Debug, Clone, Copy)]
pub struct Binding {
    pub action: &'static str,
    pub title: &'static str,
    pub keys: &'static [&'static str],
    pub shortcut: Shortcut,
}

/// The window's shortcuts (context `terminal`).
pub const BINDINGS: &[Binding] = &[
    Binding {
        action: "clipboard.copy",
        title: "Copy the selection",
        keys: &["Ctrl+Shift+C", "Ctrl+Insert"],
        shortcut: Shortcut::Copy,
    },
    Binding {
        action: "clipboard.paste",
        title: "Paste",
        keys: &["Ctrl+Shift+V", "Shift+Insert"],
        shortcut: Shortcut::Paste,
    },
    Binding {
        action: "view.zoom-in",
        title: "Larger text",
        keys: &["Ctrl+Plus", "Ctrl+NumpadAdd"],
        shortcut: Shortcut::ZoomIn,
    },
    Binding {
        action: "view.zoom-out",
        title: "Smaller text",
        keys: &["Ctrl+Minus", "Ctrl+NumpadSubtract"],
        shortcut: Shortcut::ZoomOut,
    },
    Binding {
        action: "view.zoom-reset",
        title: "Default text size",
        keys: &["Ctrl+0", "Ctrl+Numpad0"],
        shortcut: Shortcut::ZoomReset,
    },
    Binding {
        action: "scroll.page-up",
        title: "Scroll one page back",
        keys: &["Ctrl+Shift+PageUp"],
        shortcut: Shortcut::ScrollPageUp,
    },
    Binding {
        action: "scroll.page-down",
        title: "Scroll one page forward",
        keys: &["Ctrl+Shift+PageDown"],
        shortcut: Shortcut::ScrollPageDown,
    },
    Binding {
        action: "scroll.line-up",
        title: "Scroll one line back",
        keys: &["Ctrl+Shift+Up"],
        shortcut: Shortcut::ScrollLineUp,
    },
    Binding {
        action: "scroll.line-down",
        title: "Scroll one line forward",
        keys: &["Ctrl+Shift+Down"],
        shortcut: Shortcut::ScrollLineDown,
    },
    Binding {
        action: "scroll.top",
        title: "Scroll to the oldest line",
        keys: &["Ctrl+Shift+Home"],
        shortcut: Shortcut::ScrollTop,
    },
    Binding {
        action: "scroll.bottom",
        title: "Scroll to the live screen",
        keys: &["Ctrl+Shift+End"],
        shortcut: Shortcut::ScrollBottom,
    },
];

/// One shortcut as a virtual key and the exact modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Compiled {
    vk: u32,
    mods: Mods,
    shortcut: Shortcut,
}

/// The shortcut table, compiled once from [`BINDINGS`].
#[derive(Debug, Clone)]
pub struct Shortcuts(Vec<Compiled>);

impl Default for Shortcuts {
    fn default() -> Self {
        let mut v = Vec::new();
        for b in BINDINGS {
            for k in b.keys {
                let chord = Chord::parse(k).expect("built-in shortcut keys parse");
                let (m, vk) = win32_chord(&chord).expect("built-in shortcuts are plain chords");
                v.push(Compiled {
                    vk,
                    mods: Mods {
                        alt: m & 0x1 != 0,
                        ctrl: m & 0x2 != 0,
                        shift: m & 0x4 != 0,
                    },
                    shortcut: b.shortcut,
                });
            }
        }
        Self(v)
    }
}

impl Shortcuts {
    /// The shortcut a key press runs, if any.
    pub fn lookup(&self, vk: u32, mods: Mods) -> Option<Shortcut> {
        self.0
            .iter()
            .find(|c| c.vk == vk && c.mods == mods)
            .map(|c| c.shortcut)
    }
}

/// Virtual-key codes the window reads (documented `VK_*` values).
pub mod vk {
    pub const RETURN: u32 = 0x0D;
    pub const SHIFT: u32 = 0x10;
    pub const CONTROL: u32 = 0x11;
    pub const MENU: u32 = 0x12;
    pub const SPACE: u32 = 0x20;
    pub const PRIOR: u32 = 0x21;
    pub const NEXT: u32 = 0x22;
    pub const END: u32 = 0x23;
    pub const HOME: u32 = 0x24;
    pub const LEFT: u32 = 0x25;
    pub const UP: u32 = 0x26;
    pub const RIGHT: u32 = 0x27;
    pub const DOWN: u32 = 0x28;
    pub const INSERT: u32 = 0x2D;
    pub const DELETE: u32 = 0x2E;
    pub const F1: u32 = 0x70;
    pub const F4: u32 = 0x73;
    pub const F10: u32 = 0x79;
    pub const F12: u32 = 0x7B;
    pub const PROCESSKEY: u32 = 0xE5;
}

/// The pane key of a key that produces no character (cursor and editing keys, F1–F12). Keys that
/// type something arrive as characters ([`char_key`]).
pub fn special_key(code: u32) -> Option<Key> {
    Some(match code {
        vk::UP => Key::Up,
        vk::DOWN => Key::Down,
        vk::LEFT => Key::Left,
        vk::RIGHT => Key::Right,
        vk::HOME => Key::Home,
        vk::END => Key::End,
        vk::PRIOR => Key::PageUp,
        vk::NEXT => Key::PageDown,
        vk::INSERT => Key::Insert,
        vk::DELETE => Key::Delete,
        vk::F1..=vk::F12 => Key::F(u8::try_from(code - vk::F1 + 1).ok()?),
        _ => return None,
    })
}

/// `Ctrl+2` … `Ctrl+8` produce no character on most layouts, but xterm sends the VT220 control
/// codes for them (NUL, ESC, FS, GS, RS, US, DEL): the digit to send with Ctrl, if it is one.
pub fn ctrl_digit(code: u32, mods: Mods) -> Option<char> {
    (mods == Mods::CTRL && (0x32..=0x38).contains(&code)).then(|| char::from(code as u8))
}

/// What a typed character sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Typed {
    /// Encode this key with these modifiers.
    Key(Key, Mods),
    /// Send these bytes as they are (a control character the layout already produced).
    Raw(Vec<u8>),
}

/// The pane input for a character from `WM_CHAR` (`alt` = it came as `WM_SYSCHAR`), with the
/// modifiers held. `Ctrl+Alt` is AltGr on many layouts: such a character is plain text.
pub fn char_key(c: char, alt: bool, held: Mods) -> Typed {
    let altgr = held.ctrl && held.alt;
    match c {
        '\r' => Typed::Key(Key::Enter, Mods { alt, ..Mods::NONE }),
        // Ctrl+Enter arrives as LF.
        '\n' => Typed::Key(Key::Enter, Mods::NONE),
        '\t' => Typed::Key(
            Key::Tab,
            Mods {
                shift: held.shift,
                ..Mods::NONE
            },
        ),
        '\u{8}' => Typed::Key(Key::Backspace, Mods { alt, ..Mods::NONE }),
        // Ctrl+Backspace arrives as DEL; xterm sends BS for it.
        '\u{7f}' => Typed::Key(Key::Backspace, Mods::CTRL),
        '\u{1b}' => Typed::Key(Key::Escape, Mods { alt, ..Mods::NONE }),
        // Ctrl+letter (and Ctrl+[ \ ] ^ _) arrive as their C0 control already.
        c if (c as u32) < 0x20 => Typed::Raw(vec![c as u8]),
        // Ctrl+Space: NUL.
        ' ' if held.ctrl && !altgr => Typed::Key(Key::Char(' '), Mods::CTRL),
        c => Typed::Key(Key::Char(c), Mods { alt, ..Mods::NONE }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kim_hotkeys::Scope;
    use kim_hotkeys::reserved;

    #[test]
    fn every_default_parses_and_is_allowed_in_the_terminal_context() {
        let terminal = Scope::Context("terminal".into());
        let mut seen = std::collections::HashSet::new();
        for b in BINDINGS {
            assert!(kim_hotkeys::keys::is_action_id(b.action), "{}", b.action);
            for k in b.keys {
                let c = Chord::parse(k).unwrap_or_else(|e| panic!("{k}: {e:?}"));
                assert_eq!(
                    reserved::chord(&c, &terminal, true),
                    None,
                    "{k} is reserved"
                );
                assert!(win32_chord(&c).is_ok(), "{k}");
                assert!(seen.insert(*k), "{k} bound twice");
            }
        }
    }

    #[test]
    fn shortcuts_match_exact_modifiers_only() {
        let s = Shortcuts::default();
        let cs = Mods {
            ctrl: true,
            shift: true,
            alt: false,
        };
        assert_eq!(s.lookup(u32::from(b'C'), cs), Some(Shortcut::Copy));
        assert_eq!(s.lookup(u32::from(b'V'), cs), Some(Shortcut::Paste));
        assert_eq!(s.lookup(vk::INSERT, Mods::SHIFT), Some(Shortcut::Paste));
        assert_eq!(s.lookup(vk::INSERT, Mods::CTRL), Some(Shortcut::Copy));
        assert_eq!(s.lookup(0xBB, Mods::CTRL), Some(Shortcut::ZoomIn)); // VK_OEM_PLUS
        assert_eq!(s.lookup(0xBD, Mods::CTRL), Some(Shortcut::ZoomOut)); // VK_OEM_MINUS
        assert_eq!(
            s.lookup(u32::from(b'0'), Mods::CTRL),
            Some(Shortcut::ZoomReset)
        );
        assert_eq!(s.lookup(vk::PRIOR, cs), Some(Shortcut::ScrollPageUp));
        // Ctrl+C is the program's (interrupt), Ctrl+V too; Shift+PageUp is not bound.
        assert_eq!(s.lookup(u32::from(b'C'), Mods::CTRL), None);
        assert_eq!(s.lookup(u32::from(b'V'), Mods::CTRL), None);
        assert_eq!(s.lookup(vk::PRIOR, Mods::SHIFT), None);
        // Ctrl+B goes to the program until the prefix exists (Step 23).
        assert_eq!(s.lookup(u32::from(b'B'), Mods::CTRL), None);
    }

    #[test]
    fn characters_map_to_xterm_keys() {
        let none = Mods::NONE;
        assert_eq!(char_key('a', false, none), Typed::Key(Key::Char('a'), none));
        assert_eq!(
            char_key('한', false, none),
            Typed::Key(Key::Char('한'), none)
        );
        assert_eq!(
            char_key('x', true, Mods::ALT),
            Typed::Key(Key::Char('x'), Mods::ALT)
        );
        assert_eq!(char_key('\r', false, none), Typed::Key(Key::Enter, none));
        assert_eq!(
            char_key('\t', false, Mods::SHIFT),
            Typed::Key(Key::Tab, Mods::SHIFT)
        );
        assert_eq!(char_key('\u{3}', false, Mods::CTRL), Typed::Raw(vec![3]));
        assert_eq!(
            char_key('\u{7f}', false, Mods::CTRL),
            Typed::Key(Key::Backspace, Mods::CTRL)
        );
        assert_eq!(
            char_key(' ', false, Mods::CTRL),
            Typed::Key(Key::Char(' '), Mods::CTRL)
        );
        // AltGr (Ctrl+Alt) characters are text: `@` on a German layout.
        let altgr = Mods {
            ctrl: true,
            alt: true,
            shift: false,
        };
        assert_eq!(
            char_key('@', false, altgr),
            Typed::Key(Key::Char('@'), none)
        );
        assert_eq!(special_key(vk::F1), Some(Key::F(1)));
        assert_eq!(special_key(vk::F12), Some(Key::F(12)));
        assert_eq!(special_key(u32::from(b'A')), None);
        assert_eq!(ctrl_digit(0x32, Mods::CTRL), Some('2'));
        assert_eq!(ctrl_digit(0x30, Mods::CTRL), None);
    }
}
