//! Hotstrings: typed abbreviations as triggers (docs/DDALGAK_RULE_MODEL.md §3.4;
//! `DDALGAK-FR-070`, `DDALGAK-SEC-001`, `DDALGAK-SEC-002`; ADR 0034).
//!
//! The engine core keeps a **bounded buffer** ([`Buffer`]) of the last characters typed into the
//! focused control: at most the longest active abbreviation plus one character (for the word
//! boundary). It exists only while a hotstring rule is active, is cleared on every reset (focus
//! change, a password field, an IME composition, the secure desktop, pause, navigation and other
//! non-text keys, shortcuts with Ctrl, Alt or Win, input from other programs, after a match and
//! after [`IDLE_MS`] without typing), and is never stored, logged, sent or shown anywhere. Only
//! the core's own matching reads it (`DDALGAK-SEC-001`).
//!
//! Characters come from the platform ([`Translate`]): on Windows `ToUnicodeEx` with the "do not
//! change keyboard state" flag (Windows 10 1607 and later, *documented*), so translating never
//! disturbs the focused application's dead-key state (ADR 0034 refines ADR 0022 here). A dead
//! key clears the buffer: abbreviations typed with dead keys are not recognized.
//!
//! [`find`] decides a match for the next character, allocation-free: the abbreviation must end
//! the buffer (followed by an end character, or complete with this character for `immediate`
//! hotstrings), in the case the rule asks for, and, unless `inside_words`, start a word.

use crate::text::CaseForm;
use std::sync::Arc;

/// The longest abbreviation (schema `hotstring.abbreviation`; AutoHotkey allows 40 too).
pub const MAX_ABBREVIATION: usize = 40;
/// The buffer's hard bound: the longest abbreviation and the character before it.
pub const BUFFER: usize = MAX_ABBREVIATION + 1;
/// A pause in typing longer than this clears the buffer.
pub const IDLE_MS: u32 = 10_000;
/// The default end characters (AutoHotkey's default set): `-()[]{}':;"/\,.?!`, space, tab and
/// Enter (`\n`).
pub const DEFAULT_END_CHARS: &str = "-()[]{}':;\"/\\,.?! \t\n";

/// What a key types, as the platform reports it for the focused window's layout.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Typed {
    /// A character (Enter is `\n`, Tab `\t`).
    Char(char),
    /// A dead key (`^` on a French layout): its character depends on the next key.
    Dead,
    /// The key types nothing (navigation, function keys, …).
    Nothing,
}

/// The platform's key translation: virtual key, scan code, Shift held, AltGr held. A plain
/// function, so the hook callback calls it without allocating.
pub type Translate = fn(vk: u16, scan: u16, shift: bool, altgr: bool) -> Typed;

/// How case decides a match and the replacement (schema `hotstring.case`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Case {
    /// Any case matches; the replacement follows the typed case (the default).
    #[default]
    Conform,
    /// Any case matches; the replacement as written.
    Insensitive,
    /// Only the exact case matches.
    Sensitive,
}

impl Case {
    pub fn parse(s: &str) -> Option<Case> {
        match s {
            "conform" => Some(Case::Conform),
            "insensitive" => Some(Case::Insensitive),
            "sensitive" => Some(Case::Sensitive),
            _ => None,
        }
    }
}

/// A compiled hotstring trigger.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Spec {
    pub abbreviation: Vec<char>,
    pub case: Case,
    pub inside_words: bool,
    pub immediate: bool,
    pub end_chars: Vec<char>,
    pub omit_end: bool,
    pub erase: bool,
}

impl Spec {
    /// A spec with the default options.
    pub fn new(abbreviation: &str) -> Spec {
        Spec {
            abbreviation: abbreviation.chars().collect(),
            case: Case::Conform,
            inside_words: false,
            immediate: false,
            end_chars: DEFAULT_END_CHARS.chars().collect(),
            omit_end: false,
            erase: true,
        }
    }

    /// Whether the key that completes this hotstring is consumed (and given back if nothing is
    /// typed): always for an end character; for `immediate`, only when the abbreviation is erased
    /// (otherwise the key reaches the application and the replacement follows it).
    pub fn consumes(&self) -> bool {
        !self.immediate || self.erase
    }
}

/// The bounded buffer of the last typed characters. See the module documentation.
#[derive(Clone, Copy, Debug)]
pub struct Buffer {
    chars: [char; BUFFER],
    len: usize,
    /// The bound in use: the longest active abbreviation plus one.
    cap: usize,
    /// Time of the last character (ms, the event clock).
    pub last_ms: u32,
    /// The focus epoch the characters were typed in.
    pub epoch: u32,
}

impl Default for Buffer {
    fn default() -> Buffer {
        Buffer {
            chars: ['\0'; BUFFER],
            len: 0,
            cap: BUFFER,
            last_ms: 0,
            epoch: 0,
        }
    }
}

impl Buffer {
    /// An empty buffer bounded to `longest + 1` characters.
    pub fn bounded(longest: usize) -> Buffer {
        Buffer {
            cap: (longest + 1).clamp(1, BUFFER),
            ..Buffer::default()
        }
    }

    pub fn clear(&mut self) {
        self.chars = ['\0'; BUFFER];
        self.len = 0;
    }

    /// Appends a character, dropping the oldest when the bound is reached.
    pub fn push(&mut self, c: char) {
        if self.len == self.cap {
            self.chars.copy_within(1..self.cap, 0);
            self.len -= 1;
        }
        self.chars[self.len] = c;
        self.len += 1;
    }

    /// Backspace: forgets the last character.
    pub fn pop(&mut self) {
        if self.len > 0 {
            self.len -= 1;
            self.chars[self.len] = '\0';
        }
    }

    pub fn as_slice(&self) -> &[char] {
        &self.chars[..self.len]
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn capacity(&self) -> usize {
        self.cap
    }
}

/// A match: which hotstring (index into the table's list), how many characters the application
/// already shows (to erase), and the case form of what was typed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Hit {
    pub index: usize,
    pub typed: usize,
    pub case: CaseForm,
}

fn same(a: char, b: char, case: Case) -> bool {
    a == b || (case != Case::Sensitive && a.to_lowercase().eq(b.to_lowercase()))
}

/// The case form of the typed abbreviation (`BTW` upper, `Btw` title, `btw` as written).
fn form<'a>(typed: impl Iterator<Item = &'a char>) -> CaseForm {
    let letters = typed.filter(|c| c.is_alphabetic());
    let mut n = 0usize;
    let mut upper = 0usize;
    let mut first_upper = None;
    for c in letters {
        n += 1;
        if c.is_uppercase() {
            upper += 1;
        }
        first_upper.get_or_insert(c.is_uppercase());
    }
    if n >= 2 && upper == n {
        CaseForm::Upper
    } else if first_upper == Some(true) {
        CaseForm::Title
    } else {
        CaseForm::AsWritten
    }
}

/// Whether `abbr` ends `typed` (in the rule's case) and, unless `inside_words`, starts a word.
fn ends_with(typed: &[char], spec: &Spec) -> bool {
    let abbr = &spec.abbreviation;
    if abbr.is_empty() || typed.len() < abbr.len() {
        return false;
    }
    let start = typed.len() - abbr.len();
    let tail = &typed[start..];
    if !tail.iter().zip(abbr).all(|(t, a)| same(*t, *a, spec.case)) {
        return false;
    }
    spec.inside_words || start == 0 || !typed[start - 1].is_alphanumeric()
}

/// The first hotstring (in precedence order) that `c`, typed after the buffer, completes.
pub fn find(list: &[(u16, Arc<Spec>)], buf: &Buffer, c: char) -> Option<Hit> {
    let typed = buf.as_slice();
    for (index, (_, spec)) in list.iter().enumerate() {
        if spec.immediate {
            // The abbreviation completes with `c`: compare without building a new array.
            let Some((last, before)) = spec.abbreviation.split_last() else {
                continue;
            };
            if !same(c, *last, spec.case) || typed.len() < before.len() {
                continue;
            }
            let start = typed.len() - before.len();
            let ok = typed[start..]
                .iter()
                .zip(before)
                .all(|(t, a)| same(*t, *a, spec.case))
                && (spec.inside_words || start == 0 || !typed[start - 1].is_alphanumeric());
            if ok {
                let case = if spec.case == Case::Conform {
                    form(typed[start..].iter().chain(std::iter::once(&c)))
                } else {
                    CaseForm::AsWritten
                };
                return Some(Hit {
                    index,
                    typed: before.len(),
                    case,
                });
            }
        } else if spec.end_chars.contains(&c) && ends_with(typed, spec) {
            let start = typed.len() - spec.abbreviation.len();
            let case = if spec.case == Case::Conform {
                form(typed[start..].iter())
            } else {
                CaseForm::AsWritten
            };
            return Some(Hit {
                index,
                typed: spec.abbreviation.len(),
                case,
            });
        }
    }
    None
}

/// A US-QWERTY translation for tests and documentation examples (letters, digits, the common
/// punctuation, Space, Tab, Enter); never used by the engine on Windows.
pub fn us_qwerty(vk: u16, _scan: u16, shift: bool, altgr: bool) -> Typed {
    if altgr {
        return Typed::Nothing;
    }
    let c = match vk {
        0x41..=0x5A => {
            let l = char::from(u8::try_from(vk).unwrap_or(b'A'));
            if shift { l } else { l.to_ascii_lowercase() }
        }
        0x30..=0x39 => {
            let d = u8::try_from(vk - 0x30).unwrap_or(0);
            if shift {
                char::from(b")!@#$%^&*("[usize::from(d)])
            } else {
                char::from(b'0' + d)
            }
        }
        0x20 => ' ',
        0x09 => '\t',
        0x0D => '\n',
        0xBA => {
            if shift {
                ':'
            } else {
                ';'
            }
        }
        0xBB => {
            if shift {
                '+'
            } else {
                '='
            }
        }
        0xBC => {
            if shift {
                '<'
            } else {
                ','
            }
        }
        0xBD => {
            if shift {
                '_'
            } else {
                '-'
            }
        }
        0xBE => {
            if shift {
                '>'
            } else {
                '.'
            }
        }
        0xBF => {
            if shift {
                '?'
            } else {
                '/'
            }
        }
        0xDE => {
            if shift {
                '"'
            } else {
                '\''
            }
        }
        0xDB => {
            if shift {
                '{'
            } else {
                '['
            }
        }
        0xDD => {
            if shift {
                '}'
            } else {
                ']'
            }
        }
        // A stand-in dead key for tests.
        0xDF => return Typed::Dead,
        _ => return Typed::Nothing,
    };
    Typed::Char(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(s: &str) -> Buffer {
        let mut b = Buffer::bounded(MAX_ABBREVIATION);
        for c in s.chars() {
            b.push(c);
        }
        b
    }

    fn list(specs: Vec<Spec>) -> Vec<(u16, Arc<Spec>)> {
        specs
            .into_iter()
            .enumerate()
            .map(|(i, s)| (u16::try_from(i).unwrap(), Arc::new(s)))
            .collect()
    }

    #[test]
    fn an_end_character_completes_an_abbreviation_at_a_word_start() {
        let l = list(vec![Spec::new("btw")]);
        let hit = find(&l, &buf("btw"), ' ').unwrap();
        assert_eq!(
            (hit.index, hit.typed, hit.case),
            (0, 3, CaseForm::AsWritten)
        );
        assert!(find(&l, &buf("so btw"), '.').is_some(), "after a space");
        assert!(find(&l, &buf("(btw"), '!').is_some(), "after punctuation");
        assert!(find(&l, &buf("abtw"), ' ').is_none(), "inside a word");
        assert!(find(&l, &buf("btw"), 'x').is_none(), "not an end character");
        assert!(find(&l, &buf("bt"), ' ').is_none());
        let mut inside = Spec::new("btw");
        inside.inside_words = true;
        assert!(find(&list(vec![inside]), &buf("abtw"), ' ').is_some());
    }

    #[test]
    fn case_options_decide_the_match_and_the_replacement_form() {
        let l = list(vec![Spec::new("btw")]);
        assert_eq!(find(&l, &buf("BTW"), ' ').unwrap().case, CaseForm::Upper);
        assert_eq!(find(&l, &buf("Btw"), ' ').unwrap().case, CaseForm::Title);
        assert_eq!(
            find(&l, &buf("bTw"), ' ').unwrap().case,
            CaseForm::AsWritten
        );
        let mut ins = Spec::new("btw");
        ins.case = Case::Insensitive;
        assert_eq!(
            find(&list(vec![ins]), &buf("BTW"), ' ').unwrap().case,
            CaseForm::AsWritten
        );
        let mut sens = Spec::new("BtW");
        sens.case = Case::Sensitive;
        let l = list(vec![sens]);
        assert!(find(&l, &buf("BtW"), ' ').is_some());
        assert!(find(&l, &buf("btw"), ' ').is_none());
        assert!(find(&l, &buf("BTW"), ' ').is_none());
        // Hangul has no case: an exact match.
        let l = list(vec![Spec::new("ㄱㅅ")]);
        assert!(find(&l, &buf("ㄱㅅ"), ' ').is_some());
    }

    #[test]
    fn immediate_hotstrings_complete_with_their_last_character() {
        let mut s = Spec::new(";sig");
        s.immediate = true;
        let l = list(vec![s.clone()]);
        let hit = find(&l, &buf(";si"), 'g').unwrap();
        assert_eq!(hit.typed, 3, "the completing key never reached the app");
        assert!(find(&l, &buf(";si"), ' ').is_none());
        assert!(find(&l, &buf("(;si"), 'G').is_some(), "after punctuation");
        assert!(
            find(&l, &buf("x;si"), 'g').is_none(),
            "`x` right before: inside a word"
        );
        assert!(s.consumes());
        s.erase = false;
        assert!(
            !s.consumes(),
            "without erasing, the last key reaches the app"
        );
        assert!(Spec::new("btw").consumes());
    }

    #[test]
    fn precedence_and_custom_end_characters() {
        let mut a = Spec::new("tw");
        a.inside_words = true;
        let b = Spec::new("btw");
        let l = list(vec![a, b]);
        assert_eq!(find(&l, &buf("btw"), ' ').unwrap().index, 0, "first wins");
        let mut only_enter = Spec::new("addr");
        only_enter.end_chars = vec!['\n'];
        let l = list(vec![only_enter]);
        assert!(find(&l, &buf("addr"), ' ').is_none());
        assert!(find(&l, &buf("addr"), '\n').is_some());
        // An abbreviation may contain an end character: "e.g." completes on a space.
        let l = list(vec![Spec::new("e.g.")]);
        assert!(find(&l, &buf("e.g."), ' ').is_some());
    }

    #[test]
    fn the_buffer_is_bounded_and_forgets() {
        let mut b = Buffer::bounded(3);
        assert_eq!(b.capacity(), 4);
        for c in "abcdefgh".chars() {
            b.push(c);
        }
        assert_eq!(
            b.as_slice(),
            ['e', 'f', 'g', 'h'],
            "only the last 4 are kept"
        );
        b.pop();
        assert_eq!(b.as_slice(), ['e', 'f', 'g']);
        b.clear();
        assert!(b.is_empty());
        let mut big = Buffer::bounded(1000);
        assert_eq!(big.capacity(), BUFFER, "never more than the hard bound");
        for _ in 0..100 {
            big.push('x');
        }
        assert_eq!(big.len(), BUFFER);
    }

    #[test]
    fn the_test_layout_translates_like_us_qwerty() {
        assert_eq!(us_qwerty(0x42, 0, false, false), Typed::Char('b'));
        assert_eq!(us_qwerty(0x42, 0, true, false), Typed::Char('B'));
        assert_eq!(us_qwerty(0x31, 0, true, false), Typed::Char('!'));
        assert_eq!(us_qwerty(0x0D, 0, false, false), Typed::Char('\n'));
        assert_eq!(us_qwerty(0x25, 0, false, false), Typed::Nothing);
        assert_eq!(us_qwerty(0xDF, 0, false, false), Typed::Dead);
    }
}
