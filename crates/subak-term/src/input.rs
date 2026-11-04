//! What the user does in a client view, as the bytes the program in the pane expects (ADR 0036,
//! docs/TERMINAL_EMULATOR.md §4): keys in the xterm encoding (or win32-input-mode), pasted text,
//! mouse reports in the protocol and encoding the program asked for, and focus changes.
//!
//! Pure functions of the event and the pane's [`ModeState`]; nothing here touches the OS, so every
//! encoding is tested without a window. The windowing layer translates its own events into
//! [`Key`], [`MouseEvent`] and text, and sends the result to the Host as pane input.

use crate::screen::{ModeState, MouseEncoding, MouseTracking};

/// Modifier keys held with a key or mouse event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

impl Mods {
    pub const NONE: Self = Self {
        shift: false,
        alt: false,
        ctrl: false,
    };
    pub const SHIFT: Self = Self {
        shift: true,
        alt: false,
        ctrl: false,
    };
    pub const ALT: Self = Self {
        shift: false,
        alt: true,
        ctrl: false,
    };
    pub const CTRL: Self = Self {
        shift: false,
        alt: false,
        ctrl: true,
    };

    fn any(self) -> bool {
        self.shift || self.alt || self.ctrl
    }

    /// The xterm modifier parameter: 1 + shift + 2·alt + 4·ctrl.
    fn param(self) -> u8 {
        1 + u8::from(self.shift) + 2 * u8::from(self.alt) + 4 * u8::from(self.ctrl)
    }

    /// The modifier bits of a mouse report: shift 4, alt (meta) 8, ctrl 16.
    fn mouse_bits(self) -> u32 {
        4 * u32::from(self.shift) + 8 * u32::from(self.alt) + 16 * u32::from(self.ctrl)
    }
}

/// A key as the view receives it (after IME composition and the application shortcuts of
/// ADR 0009 had their turn).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A character key: the character the keyboard layout produced.
    Char(char),
    Enter,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    /// F1–F12.
    F(u8),
}

/// The bytes a key sends, in the xterm encoding: `SS3` cursor keys in application cursor mode,
/// `CSI 1 ; m X` and `CSI n ; m ~` with modifiers, C0 controls for Ctrl with a character, an `ESC`
/// prefix for Alt, CR (CR LF in newline mode) for Enter, DEL for Backspace (BS with Ctrl).
pub fn encode_key(key: Key, mods: Mods, modes: &ModeState) -> Vec<u8> {
    let mut out = Vec::new();
    let alt_prefix = |out: &mut Vec<u8>| {
        if mods.alt {
            out.push(0x1b);
        }
    };
    // Keys with a final letter: no modifier → CSI/SS3 letter; with → CSI 1 ; m letter.
    let letter = |out: &mut Vec<u8>, l: u8, ss3: bool| {
        if mods.any() {
            out.extend_from_slice(format!("\x1b[1;{}", mods.param()).as_bytes());
            out.push(l);
        } else {
            out.extend_from_slice(if ss3 { b"\x1bO" } else { b"\x1b[" });
            out.push(l);
        }
    };
    let tilde = |out: &mut Vec<u8>, n: u8| {
        if mods.any() {
            out.extend_from_slice(format!("\x1b[{n};{}~", mods.param()).as_bytes());
        } else {
            out.extend_from_slice(format!("\x1b[{n}~").as_bytes());
        }
    };
    match key {
        Key::Char(c) => {
            alt_prefix(&mut out);
            if mods.ctrl
                && let Some(b) = ctrl_byte(c)
            {
                out.push(b);
            } else {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
        Key::Enter => {
            alt_prefix(&mut out);
            out.push(b'\r');
            if modes.newline {
                out.push(b'\n');
            }
        }
        Key::Tab => {
            if mods.shift {
                out.extend_from_slice(b"\x1b[Z");
            } else {
                alt_prefix(&mut out);
                out.push(b'\t');
            }
        }
        Key::Backspace => {
            alt_prefix(&mut out);
            out.push(if mods.ctrl { 0x08 } else { 0x7f });
        }
        Key::Escape => {
            alt_prefix(&mut out);
            out.push(0x1b);
        }
        Key::Up => letter(&mut out, b'A', modes.app_cursor),
        Key::Down => letter(&mut out, b'B', modes.app_cursor),
        Key::Right => letter(&mut out, b'C', modes.app_cursor),
        Key::Left => letter(&mut out, b'D', modes.app_cursor),
        Key::Home => letter(&mut out, b'H', modes.app_cursor),
        Key::End => letter(&mut out, b'F', modes.app_cursor),
        Key::Insert => tilde(&mut out, 2),
        Key::Delete => tilde(&mut out, 3),
        Key::PageUp => tilde(&mut out, 5),
        Key::PageDown => tilde(&mut out, 6),
        Key::F(n @ 1..=4) => letter(&mut out, b"PQRS"[usize::from(n - 1)], true),
        Key::F(n @ 5..=12) => {
            const CODES: [u8; 8] = [15, 17, 18, 19, 20, 21, 23, 24];
            tilde(&mut out, CODES[usize::from(n - 5)]);
        }
        Key::F(_) => {}
    }
    out
}

/// The C0 control Ctrl with a character sends (`Ctrl+A` = 0x01 … `Ctrl+Z` = 0x1A, `Ctrl+@` and
/// `Ctrl+Space` = NUL, `Ctrl+[` = ESC, `Ctrl+\` `]` `^` `_`, the VT220 digits `Ctrl+2`…`Ctrl+8`,
/// `Ctrl+?` = DEL), or `None` when Ctrl does not change the character.
fn ctrl_byte(c: char) -> Option<u8> {
    Some(match c {
        'a'..='z' => c as u8 - b'a' + 1,
        'A'..='Z' => c as u8 - b'A' + 1,
        '@' | ' ' | '2' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '-' | '7' => 0x1f,
        '?' | '8' => 0x7f,
        _ => return None,
    })
}

/// One key event in win32-input-mode (mode 9001, which ConPTY asks its terminal for): the
/// `KEY_EVENT_RECORD` fields as `CSI Vk ; Sc ; Uc ; Kd ; Cs ; Rc _`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Win32Key {
    /// Virtual-key code.
    pub vk: u16,
    /// Scan code.
    pub scan: u16,
    /// The UTF-16 unit the key produced (0 for none).
    pub unit: u16,
    pub down: bool,
    /// `dwControlKeyState` flags.
    pub control: u32,
    pub repeat: u16,
}

pub fn encode_win32_key(k: &Win32Key) -> Vec<u8> {
    format!(
        "\x1b[{};{};{};{};{};{}_",
        k.vk,
        k.scan,
        k.unit,
        u8::from(k.down),
        k.control,
        k.repeat
    )
    .into_bytes()
}

/// Text pasted into the pane, as the program receives it.
///
/// Line breaks (`CR LF`, `LF`, `CR`) become CR, which is what Enter sends. Every other control
/// character — C0 except tab, DEL and C1 — is removed, so pasted text can neither end a bracketed
/// paste early (`ESC [201~`) nor smuggle escape sequences into the program (`SUBAK-SEC-013`). With
/// bracketed paste on (mode 2004) the text is wrapped in `ESC [200~` … `ESC [201~`.
pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + 12);
    if bracketed {
        out.extend_from_slice(b"\x1b[200~");
    }
    let mut it = text.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\r' => {
                if it.peek() == Some(&'\n') {
                    it.next();
                }
                out.push(b'\r');
            }
            '\n' => out.push(b'\r'),
            '\t' => out.push(b'\t'),
            c if c.is_control() => {}
            c => {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    if bracketed {
        out.extend_from_slice(b"\x1b[201~");
    }
    out
}

/// Pastes larger than this ask first (Windows Terminal's large-paste threshold).
pub const LARGE_PASTE_BYTES: usize = 5 * 1024;

/// What a paste would do, so the window can ask before sending text that would run commands or
/// that carries hidden control sequences (Step 19, docs/SUBAK_GUI.md §12.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PasteCheck {
    /// Line breaks (`CR LF`, `LF` or `CR`): each becomes Enter.
    pub breaks: usize,
    /// The text ends with a line break (its last line runs as soon as it is pasted).
    pub ends_with_break: bool,
    /// Control characters that [`encode_paste`] removes (C0 except tab and line breaks, DEL,
    /// C1), of which `escapes` are `ESC` or `CSI` — the start of an escape sequence.
    pub controls: usize,
    pub escapes: usize,
    /// The size of the text in UTF-8.
    pub bytes: usize,
    /// The program asked for bracketed paste (mode 2004): it receives the text as one paste
    /// and does not run lines as they arrive.
    pub bracketed: bool,
}

impl PasteCheck {
    pub fn of(text: &str, bracketed: bool) -> Self {
        let mut c = Self {
            bytes: text.len(),
            bracketed,
            ..Self::default()
        };
        let mut it = text.chars().peekable();
        while let Some(ch) = it.next() {
            match ch {
                '\r' => {
                    if it.peek() == Some(&'\n') {
                        it.next();
                    }
                    c.breaks += 1;
                }
                '\n' => c.breaks += 1,
                '\t' => {}
                '\x1b' | '\u{9b}' => {
                    c.controls += 1;
                    c.escapes += 1;
                }
                ch if ch.is_control() => c.controls += 1,
                _ => {}
            }
        }
        c.ends_with_break = text.ends_with(['\r', '\n']);
        c
    }

    /// Line breaks that a program would run as commands (none under bracketed paste).
    pub fn runs_lines(&self) -> bool {
        self.breaks > 0 && !self.bracketed
    }

    /// Whether the window asks before pasting: lines that would run, control characters that
    /// are removed (a sign of a disguised command), or a large paste.
    pub fn needs_confirmation(&self) -> bool {
        self.runs_lines() || self.controls > 0 || self.bytes > LARGE_PASTE_BYTES
    }
}

/// The "paste as one line" clean-up: surrounding blanks trimmed and every line break (with the
/// blanks around it) turned into one space, so nothing runs before the user presses Enter.
pub fn one_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for part in text.trim().split(['\r', '\n']) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(part);
    }
    out
}

/// A mouse button, including the wheel directions xterm reports as buttons 4–7.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
    WheelLeft,
    WheelRight,
}

impl MouseButton {
    fn code(self) -> u32 {
        match self {
            Self::Left => 0,
            Self::Middle => 1,
            Self::Right => 2,
            Self::WheelUp => 64,
            Self::WheelDown => 65,
            Self::WheelLeft => 66,
            Self::WheelRight => 67,
        }
    }

    pub fn is_wheel(self) -> bool {
        matches!(
            self,
            Self::WheelUp | Self::WheelDown | Self::WheelLeft | Self::WheelRight
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    Press(MouseButton),
    Release(MouseButton),
    Move,
}

/// A mouse event over the terminal grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseEvent {
    pub action: MouseAction,
    /// The cell under the pointer, from 0.
    pub col: u16,
    pub row: u16,
    /// The pointer in device pixels from the grid's top-left corner (mode 1016 only).
    pub x_px: u32,
    pub y_px: u32,
    pub mods: Mods,
}

/// The report for a mouse event under the program's tracking mode and encoding, or `None` when
/// the program did not ask for this event (or its coordinates cannot be encoded: beyond 223 in
/// the default encoding, beyond 2015 in UTF-8, as in xterm). `held` is the button held during a
/// motion, if any.
pub fn encode_mouse(
    ev: &MouseEvent,
    held: Option<MouseButton>,
    modes: &ModeState,
) -> Option<Vec<u8>> {
    let tracking = modes.mouse;
    let (mut code, release, button) = match ev.action {
        MouseAction::Press(b) => (b.code(), false, Some(b)),
        MouseAction::Release(b) => {
            if b.is_wheel() || tracking == MouseTracking::X10 {
                return None;
            }
            (b.code(), true, Some(b))
        }
        MouseAction::Move => {
            let wanted = match tracking {
                MouseTracking::AnyEvent => true,
                MouseTracking::ButtonEvent => held.is_some(),
                _ => false,
            };
            if !wanted {
                return None;
            }
            (32 + held.map_or(3, MouseButton::code), false, held)
        }
    };
    match tracking {
        MouseTracking::Off => return None,
        MouseTracking::X10 => {
            if !matches!(
                button,
                Some(MouseButton::Left | MouseButton::Middle | MouseButton::Right)
            ) {
                return None;
            }
        }
        _ => code += ev.mods.mouse_bits(),
    }
    let (x, y) = (u32::from(ev.col) + 1, u32::from(ev.row) + 1);
    let out = match modes.mouse_encoding {
        MouseEncoding::Sgr | MouseEncoding::SgrPixels => {
            let (x, y) = if modes.mouse_encoding == MouseEncoding::SgrPixels {
                (ev.x_px + 1, ev.y_px + 1)
            } else {
                (x, y)
            };
            format!("\x1b[<{code};{x};{y}{}", if release { 'm' } else { 'M' }).into_bytes()
        }
        MouseEncoding::Urxvt => {
            let code = if release { 3 + (code & !3) } else { code };
            format!("\x1b[{};{x};{y}M", code + 32).into_bytes()
        }
        MouseEncoding::Default | MouseEncoding::Utf8 => {
            // A release is button 3 with the modifier bits kept.
            let code = if release { 3 | (code & !3 & !64) } else { code };
            let mut out = b"\x1b[M".to_vec();
            for v in [code + 32, x + 32, y + 32] {
                if modes.mouse_encoding == MouseEncoding::Utf8 {
                    let c = char::from_u32(v).filter(|_| v < 0x800)?;
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                } else {
                    out.push(u8::try_from(v).ok()?);
                }
            }
            out
        }
    };
    Some(out)
}

/// The focus report (`CSI I` / `CSI O`) when the program asked for focus events (mode 1004).
pub fn encode_focus(focused: bool, modes: &ModeState) -> Option<Vec<u8>> {
    modes.focus_events.then(|| {
        if focused {
            b"\x1b[I".to_vec()
        } else {
            b"\x1b[O".to_vec()
        }
    })
}
