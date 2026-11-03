//! Canonical key model, parser and serializer (docs/HOTKEY_CONTRACT.md §5, §6.1).
//!
//! A binding's keys are a [`Sequence`] of one to four [`Step`]s. A step is a [`Chord`] (zero or
//! more modifiers plus one key), or `@<leader>`. Keys are logical (`Z`, `F1`), physical
//! (`code:KeyZ`) or characters (`char:?`). Stored and printed text is always canonical
//! ([`Sequence::parse`] accepts only that); [`Sequence::parse_input`] also accepts the aliases of
//! §5.6 and returns the canonical form. No platform key codes appear here (`KIM-NFR-022`).

use std::fmt;

/// Most steps a sequence may have (§5.3).
pub const MAX_STEPS: usize = 4;

/// Logical key names (§5.2), in canonical spelling.
pub const NAMED_KEYS: &[&str] = &[
    "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P", "Q", "R", "S",
    "T", "U", "V", "W", "X", "Y", "Z", "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "F1", "F2",
    "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12", "F13", "F14", "F15", "F16", "F17",
    "F18", "F19", "F20", "F21", "F22", "F23", "F24", "Escape", "Tab", "Enter", "Space", "Backspace",
    "Insert", "Delete", "Home", "End", "PageUp", "PageDown", "Up", "Down", "Left", "Right", "Plus",
    "Minus", "Comma", "Period", "Numpad0", "Numpad1", "Numpad2", "Numpad3", "Numpad4", "Numpad5",
    "Numpad6", "Numpad7", "Numpad8", "Numpad9", "NumpadAdd", "NumpadSubtract", "NumpadMultiply",
    "NumpadDivide", "NumpadDecimal", "NumpadEnter", "PrintScreen", "Pause", "ScrollLock",
    "ContextMenu", "MediaPlayPause", "MediaStop", "MediaNext", "MediaPrevious", "VolumeUp",
    "VolumeDown", "VolumeMute", "HangulMode", "Hanja", "KanaMode", "Convert", "NonConvert",
];

/// Physical key names after `code:` (W3C UI Events `code` values, §5.2).
pub const CODE_KEYS: &[&str] = &[
    "KeyA", "KeyB", "KeyC", "KeyD", "KeyE", "KeyF", "KeyG", "KeyH", "KeyI", "KeyJ", "KeyK", "KeyL",
    "KeyM", "KeyN", "KeyO", "KeyP", "KeyQ", "KeyR", "KeyS", "KeyT", "KeyU", "KeyV", "KeyW", "KeyX",
    "KeyY", "KeyZ", "Digit0", "Digit1", "Digit2", "Digit3", "Digit4", "Digit5", "Digit6", "Digit7",
    "Digit8", "Digit9", "Minus", "Equal", "BracketLeft", "BracketRight", "Backslash", "Semicolon",
    "Quote", "Backquote", "Comma", "Period", "Slash", "IntlBackslash", "IntlRo", "IntlYen", "Lang1",
    "Lang2", "Lang3", "Lang4", "Lang5",
];

/// Input-method mode keys, always reserved (§7.3).
pub const IME_KEYS: &[&str] = &["HangulMode", "Hanja", "KanaMode", "Convert", "NonConvert"];

/// Which physical modifier keys a modifier accepts.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum Side {
    /// Not held.
    #[default]
    None,
    /// Either side (`Ctrl`).
    Any,
    /// Only the left key (`LCtrl`).
    Left,
    /// Only the right key (`RCtrl`).
    Right,
}

impl Side {
    pub fn on(self) -> bool {
        self != Side::None
    }

    /// Whether two modifier requirements can be satisfied by the same key press.
    pub fn overlaps(self, other: Side) -> bool {
        match (self, other) {
            (Side::None, Side::None) => true,
            (Side::None, _) | (_, Side::None) => false,
            (Side::Any, _) | (_, Side::Any) => true,
            (a, b) => a == b,
        }
    }

    fn prefix(self) -> &'static str {
        match self {
            Side::Left => "L",
            Side::Right => "R",
            _ => "",
        }
    }
}

/// The modifiers of a chord. `AltGr` never combines with `Ctrl` or `Alt` (§5.1).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub struct Mods {
    pub ctrl: Side,
    pub alt: Side,
    pub altgr: bool,
    pub shift: Side,
    pub win: Side,
}

impl Mods {
    pub fn overlaps(&self, o: &Mods) -> bool {
        self.ctrl.overlaps(o.ctrl)
            && self.alt.overlaps(o.alt)
            && self.altgr == o.altgr
            && self.shift.overlaps(o.shift)
            && self.win.overlaps(o.win)
    }

    /// Whether any modifier names one side (`LCtrl`): only a hook backend can tell (§4.2).
    pub fn side_specific(&self) -> bool {
        [self.ctrl, self.alt, self.shift, self.win]
            .iter()
            .any(|s| matches!(s, Side::Left | Side::Right))
    }

    /// A "command" modifier makes a chord a shortcut rather than typing (§7.2).
    pub fn has_command(&self) -> bool {
        self.ctrl.on() || self.alt.on() || self.altgr || self.win.on()
    }

    /// Exactly these generic families are held (sides ignored), without AltGr or Win.
    pub fn exactly(&self, ctrl: bool, alt: bool, shift: bool) -> bool {
        self.ctrl.on() == ctrl
            && self.alt.on() == alt
            && self.shift.on() == shift
            && !self.altgr
            && !self.win.on()
    }

    pub fn is_empty(&self) -> bool {
        *self == Mods::default()
    }
}

/// One key: logical (named), physical (`code:`) or a produced character (`char:`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Key {
    Named(&'static str),
    Code(&'static str),
    Char(char),
}

impl Key {
    /// Whether pressing this key without Ctrl, Alt or Win types text (§7.2).
    pub fn is_typing(&self) -> bool {
        match self {
            Key::Char(_) | Key::Code(_) => true,
            Key::Named(n) => {
                n.len() == 1 || ["Plus", "Minus", "Comma", "Period"].contains(n) || n.starts_with("Numpad")
            }
        }
    }

    /// Input-method mode keys (§7.3).
    pub fn is_ime(&self) -> bool {
        match self {
            Key::Named(n) => IME_KEYS.contains(n),
            Key::Code(c) => c.starts_with("Lang"),
            Key::Char(_) => false,
        }
    }

    /// Keys that produce a printable character on some layout (for `altgr_collision`).
    pub fn is_printable(&self) -> bool {
        match self {
            Key::Named(n) => n.len() == 1 || ["Plus", "Minus", "Comma", "Period"].contains(n),
            Key::Code(_) => true,
            Key::Char(_) => false,
        }
    }

    pub fn is_named(&self, name: &str) -> bool {
        matches!(self, Key::Named(n) if *n == name)
    }
}

/// One key with the modifiers held together with it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct Chord {
    pub mods: Mods,
    pub key: Key,
}

impl Chord {
    /// Parses one canonical chord (no `char:`, no leader), e.g. a leader's keys.
    pub fn parse(text: &str) -> Result<Chord, ParseError> {
        match parse_step(text, false)? {
            Step::Chord(c) if !matches!(c.key, Key::Char(_)) => Ok(c),
            _ => Err(ParseError::new(text, "a single chord is expected here")),
        }
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let m = &self.mods;
        let alt_gr = if m.altgr { Side::Any } else { Side::None };
        for (side, name) in [
            (m.ctrl, "Ctrl"),
            (m.alt, "Alt"),
            (alt_gr, "AltGr"),
            (m.shift, "Shift"),
            (m.win, "Win"),
        ] {
            if side.on() {
                write!(f, "{}{name}+", side.prefix())?;
            }
        }
        match self.key {
            Key::Named(n) => f.write_str(n),
            Key::Code(c) => write!(f, "code:{c}"),
            Key::Char(c) => write!(f, "char:{c}"),
        }
    }
}

/// One step of a sequence.
#[derive(Clone, PartialEq, Eq, Debug, Hash)]
pub enum Step {
    Chord(Chord),
    /// `@<leader-id>`: stands for the leader's current chord (§3.3).
    Leader(String),
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Step::Chord(c) => c.fmt(f),
            Step::Leader(l) => write!(f, "@{l}"),
        }
    }
}

/// The keys of a binding: one to four steps.
#[derive(Clone, PartialEq, Eq, Debug, Hash)]
pub struct Sequence(Vec<Step>);

impl Sequence {
    /// Parses canonical text only (stored files, manifests). Anything else is `hotkey_invalid`.
    pub fn parse(text: &str) -> Result<Sequence, ParseError> {
        let parts: Vec<&str> = text.split(' ').collect();
        if parts.len() > MAX_STEPS {
            return Err(ParseError::new(text, "a sequence has at most four steps"));
        }
        let steps = parts
            .iter()
            .map(|p| parse_step(p, false))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| ParseError::new(text, &e.message))?;
        Ok(Sequence(steps))
    }

    /// Parses user input with the aliases of §5.6 (`ctrl+t`, `Shift+Ctrl+T`, `C-b`, `Esc`,
    /// `Ctrl+B c`) and returns the canonical sequence. Unknown names fail; nothing is guessed.
    pub fn parse_input(text: &str) -> Result<Sequence, ParseError> {
        let parts: Vec<&str> = text.split_whitespace().collect();
        if parts.is_empty() {
            return Err(ParseError::new(text, "no keys given"));
        }
        if parts.len() > MAX_STEPS {
            return Err(ParseError::new(text, "a sequence has at most four steps"));
        }
        let steps = parts
            .iter()
            .enumerate()
            .map(|(i, p)| parse_input_step(p, i > 0))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| ParseError::new(text, &e.message))?;
        Ok(Sequence(steps))
    }

    pub fn from_steps(steps: Vec<Step>) -> Sequence {
        Sequence(steps)
    }

    pub fn steps(&self) -> &[Step] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The single chord of a one-step sequence that is not a `char:` key.
    pub fn single_chord(&self) -> Option<&Chord> {
        match self.0.as_slice() {
            [Step::Chord(c)] if !matches!(c.key, Key::Char(_)) => Some(c),
            _ => None,
        }
    }

    /// The leader table this sequence belongs to (`@prefix …`), if it has more steps.
    pub fn table(&self) -> Option<&str> {
        match self.0.as_slice() {
            [Step::Leader(l), _, ..] => Some(l),
            _ => None,
        }
    }
}

impl fmt::Display for Sequence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, s) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            s.fmt(f)?;
        }
        Ok(())
    }
}

/// Bad key syntax (`hotkey_invalid`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ParseError {
    pub input: String,
    pub message: String,
}

impl ParseError {
    fn new(input: &str, message: &str) -> ParseError {
        ParseError {
            input: input.to_string(),
            message: message.to_string(),
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "`{}`: {}", self.input, self.message)
    }
}

impl std::error::Error for ParseError {}

/// `^[a-z][a-z0-9-]{0,31}$`: leader and context IDs (§3.2, §3.3).
pub fn is_name_id(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 32
        && b[0].is_ascii_lowercase()
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// `^[a-z][a-z0-9-]*(\.[a-z][a-z0-9-]*){1,3}$`: action IDs (§3.1).
pub fn is_action_id(s: &str) -> bool {
    let segs: Vec<&str> = s.split('.').collect();
    (2..=4).contains(&segs.len())
        && segs.iter().all(|g| {
            let b = g.as_bytes();
            !b.is_empty()
                && b[0].is_ascii_lowercase()
                && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
        })
}

/// `^[a-z][a-z0-9-]{1,31}$`: tool IDs.
pub fn is_tool_id(s: &str) -> bool {
    s.len() >= 2 && is_name_id(s)
}

fn lookup(list: &[&'static str], s: &str) -> Option<&'static str> {
    list.iter().copied().find(|k| *k == s)
}

fn lookup_ci(list: &[&'static str], s: &str) -> Option<&'static str> {
    list.iter().copied().find(|k| k.eq_ignore_ascii_case(s))
}

/// One canonical step. `lenient` is unused here; kept for symmetry with input parsing.
fn parse_step(s: &str, _lenient: bool) -> Result<Step, ParseError> {
    if let Some(l) = s.strip_prefix('@') {
        return if is_name_id(l) {
            Ok(Step::Leader(l.to_string()))
        } else {
            Err(ParseError::new(s, "a leader is written @<id> (lowercase)"))
        };
    }
    if let Some(c) = s.strip_prefix("char:") {
        return single_char(s, c);
    }
    let parts: Vec<&str> = s.split('+').collect();
    let (key, mods_text) = parts
        .split_last()
        .ok_or_else(|| ParseError::new(s, "empty step"))?;
    let mut m = Mods::default();
    let mut rank = 0;
    for token in mods_text {
        let (side, family) = match token.as_bytes().first() {
            Some(b'L') if token.len() > 1 => (Side::Left, &token[1..]),
            Some(b'R') if token.len() > 1 => (Side::Right, &token[1..]),
            _ => (Side::Any, *token),
        };
        let r = match family {
            "Ctrl" => 1,
            "Alt" => 2,
            "AltGr" if side == Side::Any => {
                if rank >= 1 {
                    return Err(ParseError::new(s, "AltGr never combines with Ctrl or Alt"));
                }
                m.altgr = true;
                rank = 2;
                continue;
            }
            "Shift" => 3,
            "Win" => 4,
            _ => return Err(ParseError::new(s, &format!("unknown modifier `{token}`"))),
        };
        if r <= rank {
            return Err(ParseError::new(
                s,
                "modifiers must be written once, in the order Ctrl, Alt (or AltGr), Shift, Win",
            ));
        }
        if r == 2 && m.altgr {
            return Err(ParseError::new(s, "AltGr never combines with Ctrl or Alt"));
        }
        rank = r;
        *slot(&mut m, r) = side;
    }
    let key = match key.strip_prefix("code:") {
        Some(code) => Key::Code(
            lookup(CODE_KEYS, code)
                .ok_or_else(|| ParseError::new(s, &format!("unknown physical key `code:{code}`")))?,
        ),
        None if key.starts_with("char:") => {
            return Err(ParseError::new(s, "a char: key takes no modifiers"));
        }
        None => Key::Named(
            lookup(NAMED_KEYS, key)
                .ok_or_else(|| ParseError::new(s, &format!("unknown key `{key}`")))?,
        ),
    };
    Ok(Step::Chord(Chord { mods: m, key }))
}

fn slot(m: &mut Mods, rank: u8) -> &mut Side {
    match rank {
        1 => &mut m.ctrl,
        2 => &mut m.alt,
        3 => &mut m.shift,
        _ => &mut m.win,
    }
}

fn single_char(step: &str, c: &str) -> Result<Step, ParseError> {
    let mut it = c.chars();
    match (it.next(), it.next()) {
        (Some(ch), None) if !ch.is_whitespace() && !ch.is_control() => {
            Ok(Step::Chord(Chord {
                mods: Mods::default(),
                key: Key::Char(ch),
            }))
        }
        _ => Err(ParseError::new(step, "char: takes exactly one visible character")),
    }
}

/// Key-name aliases accepted as input (§5.6), lower case.
const KEY_ALIASES: &[(&str, &str)] = &[
    ("esc", "Escape"),
    ("return", "Enter"),
    ("del", "Delete"),
    ("ins", "Insert"),
    ("pgup", "PageUp"),
    ("pgdn", "PageDown"),
    ("pgdown", "PageDown"),
    ("arrowup", "Up"),
    ("arrowdown", "Down"),
    ("arrowleft", "Left"),
    ("arrowright", "Right"),
    ("prtsc", "PrintScreen"),
    ("apps", "ContextMenu"),
];

fn parse_input_step(raw: &str, after_first: bool) -> Result<Step, ParseError> {
    if let Some(l) = raw.strip_prefix('@') {
        return if is_name_id(l) {
            Ok(Step::Leader(l.to_string()))
        } else {
            Err(ParseError::new(raw, "a leader is written @<id> (lowercase)"))
        };
    }
    if raw.len() > 5 && raw[..5].eq_ignore_ascii_case("char:") {
        return single_char(raw, &raw[5..]);
    }
    let mut m = Mods::default();
    let mut rest = raw;
    // tmux style: C-b, M-x, S-Up, C-M-a.
    loop {
        let b = rest.as_bytes();
        if b.len() > 2 && b[1] == b'-' && matches!(b[0], b'C' | b'M' | b'S') {
            let family = match b[0] {
                b'C' => "ctrl",
                b'M' => "alt",
                _ => "shift",
            };
            add_modifier(&mut m, family, Side::Any, raw)?;
            rest = &rest[2..];
        } else {
            break;
        }
    }
    // A single character after the first step is a character key (`Ctrl+B c` → `char:c`),
    // unless it is a key name of its own (`0`…`9`, `A`…`Z`).
    if after_first && m.is_empty() && rest.chars().count() == 1 && lookup(NAMED_KEYS, rest).is_none() {
        return single_char(raw, rest);
    }
    let parts: Vec<&str> = rest.split('+').collect();
    let (key, mods_text) = parts
        .split_last()
        .ok_or_else(|| ParseError::new(raw, "empty step"))?;
    for token in mods_text {
        let lower = token.to_ascii_lowercase();
        let (side, family) = match lower.as_str() {
            "lctrl" | "lcontrol" => (Side::Left, "ctrl"),
            "rctrl" | "rcontrol" => (Side::Right, "ctrl"),
            "lalt" => (Side::Left, "alt"),
            "ralt" => (Side::Right, "alt"),
            "lshift" => (Side::Left, "shift"),
            "rshift" => (Side::Right, "shift"),
            "lwin" => (Side::Left, "win"),
            "rwin" => (Side::Right, "win"),
            "ctrl" | "control" | "ctl" => (Side::Any, "ctrl"),
            "alt" | "option" | "meta" => (Side::Any, "alt"),
            "altgr" => (Side::Any, "altgr"),
            "shift" => (Side::Any, "shift"),
            "win" | "windows" | "super" | "cmd" => (Side::Any, "win"),
            _ => {
                return Err(ParseError::new(
                    raw,
                    &format!("unknown modifier `{token}`"),
                ));
            }
        };
        add_modifier(&mut m, family, side, raw)?;
    }
    let key = if key.len() > 5 && key[..5].eq_ignore_ascii_case("code:") {
        Key::Code(lookup_ci(CODE_KEYS, &key[5..]).ok_or_else(|| {
            ParseError::new(raw, &format!("unknown physical key `{key}`"))
        })?)
    } else if key.len() > 5 && key[..5].eq_ignore_ascii_case("char:") {
        return Err(ParseError::new(raw, "a char: key takes no modifiers"));
    } else if let Some((_, canon)) = KEY_ALIASES.iter().find(|(a, _)| a.eq_ignore_ascii_case(key)) {
        Key::Named(lookup(NAMED_KEYS, canon).expect("alias targets are key names"))
    } else {
        Key::Named(
            lookup_ci(NAMED_KEYS, key)
                .ok_or_else(|| ParseError::new(raw, &format!("unknown key `{key}`")))?,
        )
    };
    Ok(Step::Chord(Chord { mods: m, key }))
}

fn add_modifier(m: &mut Mods, family: &str, side: Side, raw: &str) -> Result<(), ParseError> {
    let dup = || ParseError::new(raw, "a modifier is given twice");
    match family {
        "altgr" => {
            if m.altgr {
                return Err(dup());
            }
            m.altgr = true;
        }
        _ => {
            let s = match family {
                "ctrl" => &mut m.ctrl,
                "alt" => &mut m.alt,
                "shift" => &mut m.shift,
                _ => &mut m.win,
            };
            if s.on() {
                return Err(dup());
            }
            *s = side;
        }
    }
    if m.altgr && (m.ctrl.on() || m.alt.on()) {
        return Err(ParseError::new(raw, "AltGr never combines with Ctrl or Alt"));
    }
    Ok(())
}

/// Whether two steps can be triggered by the same key press (§5.5). Logical and physical keys
/// depend on the layout and are not compared; `char:` keys are compared with named keys only
/// for ASCII letters and digits (US-like layouts).
pub fn steps_overlap(a: &Step, b: &Step) -> bool {
    let (Step::Chord(x), Step::Chord(y)) = (a, b) else {
        return false;
    };
    match (x.key, y.key) {
        (Key::Char(p), Key::Char(q)) => p == q,
        (Key::Char(c), Key::Named(_)) => char_matches(c, y),
        (Key::Named(_), Key::Char(c)) => char_matches(c, x),
        (Key::Named(p), Key::Named(q)) | (Key::Code(p), Key::Code(q)) => {
            p == q && x.mods.overlaps(&y.mods)
        }
        _ => false,
    }
}

fn char_matches(c: char, chord: &Chord) -> bool {
    let Key::Named(n) = chord.key else {
        return false;
    };
    let m = &chord.mods;
    if m.has_command() {
        return false;
    }
    if c.is_ascii_alphabetic() {
        n.len() == 1 && n.starts_with(c.to_ascii_uppercase()) && m.shift.on() == c.is_ascii_uppercase()
    } else if c.is_ascii_digit() {
        n.len() == 1 && n.starts_with(c) && !m.shift.on()
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canon(s: &str) -> String {
        Sequence::parse(s).unwrap().to_string()
    }

    fn input(s: &str) -> String {
        Sequence::parse_input(s).unwrap_or_else(|e| panic!("{e}")).to_string()
    }

    #[test]
    fn canonical_text_round_trips() {
        for s in [
            "Ctrl+Shift+T",
            "LCtrl+RAlt+F5",
            "AltGr+Shift+Q",
            "Ctrl+code:KeyZ",
            "char:?",
            "F1",
            "@prefix char:c",
            "@prefix @prefix",
            "Ctrl+K Ctrl+S",
            "Ctrl+Alt+Shift+F24",
            "NumpadEnter",
            "char:한",
        ] {
            assert_eq!(canon(s), s);
        }
    }

    #[test]
    fn non_canonical_text_is_rejected() {
        for s in [
            "Shift+Ctrl+T",
            "Hyper+K",
            "ctrl+t",
            "Ctrl+AltGr+Q",
            "AltGr+Alt+Q",
            "Ctrl+Ctrl+T",
            "Ctrl+char:c",
            "char:ab",
            "char: ",
            "F25",
            "code:KeyAA",
            "A B C D E",
            "",
            "Ctrl+",
            "@Prefix",
            "Ctrl+T  F1",
        ] {
            assert!(Sequence::parse(s).is_err(), "{s} must be rejected");
        }
    }

    #[test]
    fn input_aliases_become_canonical() {
        assert_eq!(input("ctrl+t"), "Ctrl+T");
        assert_eq!(input("Shift+Ctrl+T"), "Ctrl+Shift+T");
        assert_eq!(input("Control+Option+Del"), "Ctrl+Alt+Delete");
        assert_eq!(input("C-b"), "Ctrl+B");
        assert_eq!(input("C-M-x"), "Ctrl+Alt+X");
        assert_eq!(input("S-Up"), "Shift+Up");
        assert_eq!(input("Esc"), "Escape");
        assert_eq!(input("cmd+space"), "Win+Space");
        assert_eq!(input("Super+PgDn"), "Win+PageDown");
        assert_eq!(input("Ctrl+B c"), "Ctrl+B char:c");
        assert_eq!(input("Ctrl+B ?"), "Ctrl+B char:?");
        assert_eq!(input("Ctrl+B 0"), "Ctrl+B 0");
        assert_eq!(input("@prefix c"), "@prefix char:c");
        assert_eq!(input("ctrl+code:keyz"), "Ctrl+code:KeyZ");
        assert_eq!(input("rctrl+lalt+f5"), "RCtrl+LAlt+F5");
        assert_eq!(input("  F1  "), "F1");
        for bad in ["Hyper+K", "Ctrl+Ctrl+T", "AltGr+Ctrl+Q", "Ctrl+Foo", "", "Ctrl+char:x"] {
            assert!(Sequence::parse_input(bad).is_err(), "{bad} must be rejected");
        }
    }

    #[test]
    fn generic_and_side_specific_modifiers_overlap() {
        let s = |t: &str| Sequence::parse(t).unwrap().steps()[0].clone();
        assert!(steps_overlap(&s("Ctrl+T"), &s("LCtrl+T")));
        assert!(!steps_overlap(&s("LCtrl+T"), &s("RCtrl+T")));
        assert!(!steps_overlap(&s("Ctrl+T"), &s("Ctrl+Shift+T")));
        assert!(steps_overlap(&s("char:c"), &s("C")));
        assert!(steps_overlap(&s("char:C"), &s("Shift+C")));
        assert!(!steps_overlap(&s("char:c"), &s("Shift+C")));
        assert!(steps_overlap(&s("char:1"), &s("1")));
        // Logical and physical keys are layout-dependent and never compared (§6.1).
        assert!(!steps_overlap(&s("Ctrl+Z"), &s("Ctrl+code:KeyZ")));
        assert!(steps_overlap(&s("Ctrl+code:KeyZ"), &s("Ctrl+code:KeyZ")));
        assert!(!steps_overlap(&s("@prefix"), &s("@prefix")));
    }

    #[test]
    fn ids() {
        assert!(is_action_id("catalog.refresh"));
        assert!(is_action_id("window.select-0"));
        assert!(!is_action_id("refresh"));
        assert!(!is_action_id("a.b.c.d.e"));
        assert!(!is_action_id("Catalog.refresh"));
        assert!(is_tool_id("kim") && !is_tool_id("k"));
        assert!(is_name_id("prefix") && !is_name_id("Prefix"));
    }
}
