//! Reserved keys (docs/HOTKEY_CONTRACT.md §7): chords no binding may use, by scope.

use crate::keys::{Chord, Sequence, Step};
use crate::scope::Scope;

/// Who reserves a chord, as reported in a conflict's `with.reserved`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Owner {
    Os,
    Kim,
    Ime,
    Emergency,
}

impl Owner {
    pub fn as_str(self) -> &'static str {
        match self {
            Owner::Os => "os",
            Owner::Kim => "kim",
            Owner::Ime => "ime",
            Owner::Emergency => "emergency",
        }
    }
}

/// A reserved chord that a step hits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Reservation {
    /// The conflict code: `os_reserved`, `kim_reserved` or `ime_reserved`.
    pub code: crate::check::Code,
    pub owner: Owner,
    pub name: &'static str,
}

const NAV_NO_COMMAND: [&str; 16] = [
    "Tab",
    "Escape",
    "Enter",
    "Space",
    "Up",
    "Down",
    "Left",
    "Right",
    "Home",
    "End",
    "PageUp",
    "PageDown",
    "Backspace",
    "Insert",
    "Delete",
    "ContextMenu",
];

/// The reservation a chord hits in `scope`; `first` says whether it is a binding's first step
/// (typing and navigation keys are reserved only there, §7.2).
pub fn chord(c: &Chord, scope: &Scope, first: bool) -> Option<Reservation> {
    use crate::check::Code::{ImeReserved, KimReserved, OsReserved};
    let (m, k) = (&c.mods, &c.key);
    let named = |n: &str| k.is_named(n);
    let r = |code, owner, name| Some(Reservation { code, owner, name });
    let app_or_global = matches!(scope, Scope::App | Scope::Global);
    let plain_alt = m.alt.on() && !m.altgr && !m.win.on();
    if k.is_ime() {
        r(ImeReserved, Owner::Ime, "Input method key")
    } else if m.win.on() {
        r(OsReserved, Owner::Os, "Windows key shortcut")
    } else if m.ctrl.on() && plain_alt && (named("Delete") || named("NumpadDecimal")) {
        r(OsReserved, Owner::Os, "Secure Attention Sequence")
    } else if m.exactly(true, false, true) && named("Escape") {
        r(OsReserved, Owner::Os, "Task Manager")
    } else if m.exactly(true, false, false) && named("Escape") {
        r(OsReserved, Owner::Os, "Start menu")
    } else if plain_alt && named("Tab") {
        r(OsReserved, Owner::Os, "Task switching")
    } else if plain_alt && !m.ctrl.on() && named("Escape") {
        r(OsReserved, Owner::Os, "Window cycling")
    } else if m.exactly(false, true, true) && named("PrintScreen") {
        r(OsReserved, Owner::Os, "High Contrast toggle")
    } else if m.exactly(false, true, false) && named("F4") {
        r(KimReserved, Owner::Kim, "Closes the window")
    } else if m.exactly(true, true, true) && named("Escape") {
        r(KimReserved, Owner::Emergency, "Emergency chord")
    } else if *scope == Scope::Global && named("F12") {
        r(OsReserved, Owner::Os, "Reserved for debuggers")
    } else if !(first && app_or_global) {
        None
    } else if (m.exactly(false, false, false) && (named("F6") || named("ContextMenu")))
        || (m.exactly(false, false, true) && (named("F6") || named("F10") || named("Tab")))
        || (!m.has_command() && NAV_NO_COMMAND.iter().any(|n| named(n)))
    {
        r(KimReserved, Owner::Kim, "Navigation key")
    } else if !m.has_command() && k.is_typing() {
        r(KimReserved, Owner::Kim, "Typing key")
    } else {
        None
    }
}

/// The first scope-independent reservation in any step of `seq` (what the schemas'
/// `reserved_free` rejects): checked in a context, as a later step.
pub fn scope_independent(seq: &Sequence) -> Option<Reservation> {
    let ctx = Scope::Context(String::new());
    seq.steps().iter().find_map(|s| match s {
        Step::Chord(c) => chord(c, &ctx, false),
        Step::Leader(_) => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::Code;

    #[allow(clippy::needless_pass_by_value)]
    fn r(c: &str, s: Scope) -> Option<Code> {
        let seq = Sequence::parse(c).unwrap();
        let Step::Chord(ch) = &seq.steps()[0] else {
            panic!("{c}")
        };
        chord(ch, &s, true).map(|x| x.code)
    }

    #[test]
    fn scope_dependent_reservations() {
        assert_eq!(r("F12", Scope::Global), Some(Code::OsReserved));
        assert_eq!(r("F12", Scope::App), None);
        assert_eq!(r("Tab", Scope::App), Some(Code::KimReserved));
        assert_eq!(r("Tab", Scope::Context("overlay".into())), None);
        assert_eq!(r("Shift+A", Scope::Global), Some(Code::KimReserved));
        assert_eq!(r("char:?", Scope::App), Some(Code::KimReserved));
        assert_eq!(r("F1", Scope::Global), None);
        assert_eq!(r("Ctrl+Up", Scope::App), None);
        assert_eq!(r("F6", Scope::App), Some(Code::KimReserved));
        assert_eq!(r("Shift+F10", Scope::App), Some(Code::KimReserved));
        assert_eq!(r("PrintScreen", Scope::Global), None);
        assert_eq!(r("Ctrl+Alt+Shift+F24", Scope::Global), None);
    }

    #[test]
    fn scope_independent_rules() {
        let reserved = [
            "Ctrl+Alt+Delete",
            "LCtrl+RAlt+Shift+NumpadDecimal",
            "Win+L",
            "Ctrl+LWin+D",
            "Ctrl+Shift+Escape",
            "Ctrl+Escape",
            "Alt+Tab",
            "Ctrl+Alt+Shift+Tab",
            "Alt+Escape",
            "Alt+Shift+Escape",
            "Alt+Shift+PrintScreen",
            "Alt+F4",
            "Ctrl+Alt+Shift+Escape",
            "HangulMode",
            "Ctrl+Hanja",
            "Ctrl+code:Lang1",
        ];
        for c in reserved {
            assert!(
                scope_independent(&Sequence::parse(c).unwrap()).is_some(),
                "{c} is reserved"
            );
        }
        for c in [
            "Ctrl+Alt+Escape",
            "Ctrl+Alt+F4",
            "AltGr+Tab",
            "Ctrl+T",
            "Shift+F10",
            "F12",
            "Ctrl+Shift+Delete",
            "Alt+Shift+F4",
        ] {
            assert!(
                scope_independent(&Sequence::parse(c).unwrap()).is_none(),
                "{c} is not reserved in every scope"
            );
        }
        assert!(scope_independent(&Sequence::parse("@prefix Win+L").unwrap()).is_some());
    }
}
