//! Rule trigger syntax (docs/DDALGAK_RULE_MODEL.md §3.1): the hotkey contract's canonical chords
//! and sequences, parsed by `kim-hotkeys`, plus Ddalgak's extensions — the lock keys `CapsLock`
//! and `NumLock` as keys, one bare side-specific modifier (`RAlt`, `LCtrl`, …) as a whole
//! trigger, and (Step 68, ADR 0031) the mouse buttons `MouseMiddle`, `MouseX1` and `MouseX2`,
//! optionally with modifiers, as a whole trigger — without `char:` keys, leaders or `Win` chords
//! (ADR 0022). The primary and secondary buttons are never triggers, so no rule can take away the
//! user's ability to click. Reserved chords are refused with the contract's reasons
//! (`DDALGAK-SEC-009`).

use crate::inject::Button;
use crate::vk::ModKey;
use kim_hotkeys::keys::{Chord, Key, Mods, Sequence, Side, Step};
use kim_hotkeys::reserved;
use std::fmt;

/// The key of one trigger chord.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum TKey {
    /// A logical key (`J`, `F13`): matched by virtual key, which follows the active layout.
    Named(&'static str),
    /// A physical key (`code:KeyJ`): matched by scan code, whatever the layout.
    Code(&'static str),
    /// `CapsLock` or `NumLock`.
    Lock(&'static str),
    /// A bare side-specific modifier; only as a single-step trigger.
    Modifier(ModKey),
    /// A mouse button (`MouseMiddle`, `MouseX1`, `MouseX2`); only as a single-step trigger.
    Mouse(Button),
}

/// One chord of a trigger.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct TChord {
    pub mods: Mods,
    pub key: TKey,
}

impl TChord {
    /// The chord as `kim-hotkeys` sees it, when it has an equivalent (lock keys and bare
    /// modifiers have none).
    pub fn as_contract_chord(&self) -> Option<Chord> {
        match self.key {
            TKey::Named(n) => Some(Chord {
                mods: self.mods,
                key: Key::Named(n),
            }),
            TKey::Code(c) => Some(Chord {
                mods: self.mods,
                key: Key::Code(c),
            }),
            TKey::Lock(_) | TKey::Modifier(_) | TKey::Mouse(_) => None,
        }
    }
}

impl fmt::Display for TChord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.key {
            TKey::Modifier(m) => f.write_str(m.name()),
            TKey::Lock(_) | TKey::Mouse(_) => {
                let name = match self.key {
                    TKey::Mouse(b) => b.name(),
                    TKey::Lock(n) => n,
                    _ => "",
                };
                // Print the modifiers through the contract's serializer, then the key.
                let shown = Chord {
                    mods: self.mods,
                    key: Key::Named("F1"),
                }
                .to_string();
                write!(f, "{}{name}", shown.strip_suffix("F1").unwrap_or(""))
            }
            _ => self
                .as_contract_chord()
                .map_or(Ok(()), |c| write!(f, "{c}")),
        }
    }
}

/// A parsed rule trigger: one to four chords.
#[derive(Clone, PartialEq, Eq, Debug, Hash)]
pub struct Trigger {
    steps: Vec<TChord>,
}

/// Why a trigger was refused, with the CLI error code it maps to.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TriggerError {
    /// Not the trigger syntax (`trigger_invalid`; CLI `hotkey_invalid`).
    Invalid(String),
    /// A reserved chord (`trigger_reserved`; CLI `hotkey_reserved`).
    Reserved(&'static str),
}

impl TriggerError {
    /// The validation code of DDALGAK_RULE_MODEL §8.
    pub fn code(&self) -> &'static str {
        match self {
            TriggerError::Invalid(_) => "trigger_invalid",
            TriggerError::Reserved(_) => "trigger_reserved",
        }
    }
}

impl fmt::Display for TriggerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TriggerError::Invalid(why) => f.write_str(why),
            TriggerError::Reserved(name) => write!(f, "the chord is reserved ({name})"),
        }
    }
}

const LOCK_KEYS: [&str; 2] = ["CapsLock", "NumLock"];

/// The mouse buttons that may be triggers (Step 68, ADR 0031).
pub const MOUSE_TRIGGERS: [Button; 3] = [Button::Middle, Button::X1, Button::X2];

fn parse_chord(step: &str, single: bool) -> Result<TChord, TriggerError> {
    let invalid = |why: &str| Err(TriggerError::Invalid(format!("`{step}`: {why}")));
    if let Some(m) = ModKey::from_name(step) {
        if matches!(m, ModKey::LWin | ModKey::RWin) {
            return Err(TriggerError::Reserved("Windows key shortcut"));
        }
        if !single {
            return invalid("a bare modifier is a whole trigger, never a sequence step");
        }
        return Ok(TChord {
            mods: Mods::default(),
            key: TKey::Modifier(m),
        });
    }
    let lock = LOCK_KEYS
        .iter()
        .copied()
        .find(|k| step == *k || step.ends_with(&format!("+{k}")));
    if let Some(lock) = lock {
        let prefix = &step[..step.len() - lock.len()];
        // Parse the modifiers with the contract's parser through a stand-in key.
        let chord = Chord::parse(&format!("{prefix}F1"))
            .map_err(|e| TriggerError::Invalid(format!("`{step}`: {}", e.message)))?;
        return finish(TChord {
            mods: chord.mods,
            key: TKey::Lock(lock),
        });
    }
    let button = Button::ALL
        .into_iter()
        .find(|b| step == b.name() || step.ends_with(&format!("+{}", b.name())));
    if let Some(b) = button {
        if !MOUSE_TRIGGERS.contains(&b) {
            return invalid(
                "the primary and secondary mouse buttons are never triggers (a rule must not take away clicking); use MouseMiddle, MouseX1 or MouseX2",
            );
        }
        if !single {
            return invalid("a mouse button is a whole trigger, never a sequence step");
        }
        let prefix = &step[..step.len() - b.name().len()];
        let chord = Chord::parse(&format!("{prefix}F1"))
            .map_err(|e| TriggerError::Invalid(format!("`{step}`: {}", e.message)))?;
        return finish(TChord {
            mods: chord.mods,
            key: TKey::Mouse(b),
        });
    }
    if step.starts_with("char:") {
        return invalid("character keys are not rule triggers (they would disturb dead keys)");
    }
    if step.starts_with('@') {
        return invalid("rule triggers have no leaders; write the sequence out");
    }
    let chord = Chord::parse(step)
        .map_err(|e| TriggerError::Invalid(format!("`{step}`: {}", e.message)))?;
    let key = match chord.key {
        Key::Named(n) => TKey::Named(n),
        Key::Code(c) => TKey::Code(c),
        Key::Char(_) => return invalid("character keys are not rule triggers"),
    };
    finish(TChord {
        mods: chord.mods,
        key,
    })
}

fn finish(c: TChord) -> Result<TChord, TriggerError> {
    if c.mods.win != Side::None {
        return Err(TriggerError::Reserved("Windows key shortcut"));
    }
    Ok(c)
}

impl Trigger {
    /// Parses canonical trigger text (what `rules.json` stores) and refuses reserved chords.
    pub fn parse(text: &str) -> Result<Trigger, TriggerError> {
        let parts: Vec<&str> = text.split(' ').collect();
        if parts.is_empty() || parts.len() > 4 || parts.iter().any(|p| p.is_empty()) {
            return Err(TriggerError::Invalid(format!(
                "`{text}`: a trigger has one to four chords separated by one space"
            )));
        }
        let single = parts.len() == 1;
        let steps = parts
            .iter()
            .map(|p| parse_chord(p, single))
            .collect::<Result<Vec<_>, _>>()?;
        let t = Trigger { steps };
        if let Some(name) = t.reservation() {
            return Err(TriggerError::Reserved(name));
        }
        Ok(t)
    }

    /// The first reserved chord in any step (DDALGAK_RULE_MODEL §3.1): the contract's
    /// scope-independent reservations (Secure Attention Sequence, `Win`, Task Manager, task
    /// switching, `Alt+F4`, IME keys, the emergency chord, …) plus `F12` with any modifiers.
    pub fn reservation(&self) -> Option<&'static str> {
        let chords: Vec<Step> = self
            .steps
            .iter()
            .filter_map(TChord::as_contract_chord)
            .map(Step::Chord)
            .collect();
        if let Some(r) = reserved::scope_independent(&Sequence::from_steps(chords)) {
            return Some(r.name);
        }
        self.steps
            .iter()
            .any(|c| c.key == TKey::Named("F12"))
            .then_some("Reserved for debuggers")
    }

    /// A trigger without keys: the binding of a hotstring rule (Step 69), which the core matches
    /// on typed characters instead.
    pub fn none() -> Trigger {
        Trigger { steps: Vec::new() }
    }

    pub fn steps(&self) -> &[TChord] {
        &self.steps
    }

    pub fn is_sequence(&self) -> bool {
        self.steps.len() > 1
    }

    /// The single chord of a one-step trigger.
    pub fn single(&self) -> Option<&TChord> {
        match self.steps.as_slice() {
            [c] => Some(c),
            _ => None,
        }
    }

    /// The mouse button of a mouse-button trigger (it needs the low-level mouse hook).
    pub fn mouse(&self) -> Option<Button> {
        match self.single()?.key {
            TKey::Mouse(b) => Some(b),
            _ => None,
        }
    }
}

impl fmt::Display for Trigger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, s) in self.steps.iter().enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            s.fmt(f)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(t: &str) -> Trigger {
        Trigger::parse(t).unwrap_or_else(|e| panic!("{t}: {e}"))
    }

    #[test]
    fn contract_chords_and_ddalgak_extensions_parse_and_round_trip() {
        for t in [
            "Ctrl+Shift+J",
            "F13",
            "Ctrl+Alt+Shift+F13",
            "code:KeyJ",
            "Ctrl+code:KeyZ",
            "LCtrl+RShift+F14",
            "AltGr+Q",
            "CapsLock",
            "Shift+CapsLock",
            "Ctrl+NumLock",
            "RAlt",
            "LCtrl",
            "Ctrl+K Ctrl+U",
            "F13 F14 F15 F16",
            "Escape",
            "A",
            "MouseX1",
            "Ctrl+MouseMiddle",
            "LCtrl+Alt+Shift+MouseX2",
        ] {
            assert_eq!(ok(t).to_string(), t);
        }
        assert_eq!(ok("Ctrl+MouseX1").mouse(), Some(Button::X1));
        assert_eq!(ok("F13").mouse(), None);
        assert_eq!(
            ok("RAlt").single().unwrap().key,
            TKey::Modifier(ModKey::RAlt)
        );
        assert_eq!(
            ok("Shift+CapsLock").single().unwrap().key,
            TKey::Lock("CapsLock")
        );
        assert!(ok("Shift+CapsLock").single().unwrap().mods.shift.on());
        assert!(ok("Ctrl+K Ctrl+U").is_sequence());
    }

    #[test]
    fn reserved_chords_are_refused_with_reasons() {
        for (t, why) in [
            ("Ctrl+Alt+Shift+Escape", "Emergency chord"),
            ("LCtrl+RAlt+LShift+Escape", "Emergency chord"),
            ("Ctrl+Alt+Delete", "Secure Attention Sequence"),
            ("Ctrl+Shift+Escape", "Task Manager"),
            ("Alt+Tab", "Task switching"),
            ("Alt+F4", "Closes the window"),
            ("Win+L", "Windows key shortcut"),
            ("Ctrl+Win+F13", "Windows key shortcut"),
            ("LWin", "Windows key shortcut"),
            ("F12", "Reserved for debuggers"),
            ("Ctrl+Shift+F12", "Reserved for debuggers"),
            ("HangulMode", "Input method key"),
            ("code:Lang1", "Input method key"),
            ("F13 Alt+Tab", "Task switching"),
            ("Win+MouseX1", "Windows key shortcut"),
        ] {
            assert_eq!(
                Trigger::parse(t),
                Err(TriggerError::Reserved(why)),
                "{t} should be reserved"
            );
        }
    }

    #[test]
    fn invalid_syntax_is_refused() {
        for t in [
            "",
            "ctrl+j",
            "Shift+Ctrl+J",
            "char:?",
            "@prefix c",
            "F13 RAlt",
            "F13  F14",
            "A B C D E",
            "Ctrl+CapsLockX",
            "Hyper+J",
            "MouseLeft",
            "Ctrl+MouseRight",
            "MouseX1 F13",
            "F13 MouseX2",
            "Shift+Ctrl+MouseX1",
        ] {
            let e = Trigger::parse(t).unwrap_err();
            assert_eq!(e.code(), "trigger_invalid", "{t}: {e}");
        }
    }
}
