//! Keyboard dispatch of the window (ADR 0009 order, docs/SUBAK_GUI.md §3): the IME guard, then
//! the window's own shortcuts, then everything else to the pane in the xterm encoding of
//! `subak-term`.
//!
//! The shortcuts are Subak's actions of the Kim hotkey contract (docs/HOTKEY_CONTRACT.md §13.1):
//! the shipped manifest ([`crate::hotkeys`]) layered with the user's `hotkeys.json`, matched with
//! `kim_hotkeys::keys::steps_overlap` like Kim Manager does (backend `focus` inside the window):
//! logical keys (the layout's virtual key), physical `code:` keys (the scan code) and `char:`
//! keys (the character the layout produces), sequences (a pending step waits without a timer;
//! `Esc`, a click or focus loss cancels it; an unbound next step is swallowed), and the `press`,
//! `release` and `hold` triggers. Bindings of the `terminal` context win over `app` bindings
//! with the same keys (§9.1). A key that is a shortcut never reaches the program — not even its
//! auto-repeat (`repeat: false` swallows it) — and a key that is not one always does
//! (`SUBAK-FR-034`). The `Ctrl+B` prefix and its pass-through are Step 23's: until then
//! `Ctrl+B` goes to the program, and a shadowed key such as `Ctrl+W` reaches the program only
//! after `subak hotkeys disable` or a rebind.

use crate::hotkeys as sh;
use kim_hotkeys::Scope;
use kim_hotkeys::check::{ToolState, analyze};
use kim_hotkeys::keys::{CODE_KEYS, Chord, Key as HKey, Mods as HMods, NAMED_KEYS, Step};
use kim_hotkeys::manifest::{BindingSpec, Trigger};
use kim_hotkeys::store::{self, ConfigState, Loaded};
use std::path::{Path, PathBuf};
use subak_term::{Key, Mods};

/// A window action a shortcut runs (an implemented action of Subak's manifest).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    NewTerminal,
    CloseTerminal,
    NextTab,
    PreviousTab,
    HideTab,
    MoveTabLeft,
    MoveTabRight,
    RenameTerminal,
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
    /// The Terminal manager's actions (context `manager`, Step 18).
    ManagerOpen,
    ManagerNew,
    ManagerRename,
    ManagerDelete,
    ManagerSearch,
    ManagerPin,
    ManagerClose,
}

impl Action {
    /// Every action with its manifest ID, in manifest order ([`crate::hotkeys::IMPLEMENTED`]).
    pub const ALL: [(Action, &'static str); 26] = [
        (Action::NewTerminal, "terminal.new"),
        (Action::CloseTerminal, "terminal.close"),
        (Action::NextTab, "tab.next"),
        (Action::PreviousTab, "tab.previous"),
        (Action::HideTab, "tab.hide"),
        (Action::MoveTabLeft, "tab.move-left"),
        (Action::MoveTabRight, "tab.move-right"),
        (Action::RenameTerminal, "terminal.rename"),
        (Action::Copy, "clipboard.copy"),
        (Action::Paste, "clipboard.paste"),
        (Action::ZoomIn, "view.zoom-in"),
        (Action::ZoomOut, "view.zoom-out"),
        (Action::ZoomReset, "view.zoom-reset"),
        (Action::ScrollPageUp, "scroll.page-up"),
        (Action::ScrollPageDown, "scroll.page-down"),
        (Action::ScrollLineUp, "scroll.line-up"),
        (Action::ScrollLineDown, "scroll.line-down"),
        (Action::ScrollTop, "scroll.top"),
        (Action::ScrollBottom, "scroll.bottom"),
        (Action::ManagerOpen, "manager.open"),
        (Action::ManagerNew, "manager.new"),
        (Action::ManagerRename, "manager.rename"),
        (Action::ManagerDelete, "manager.delete"),
        (Action::ManagerSearch, "manager.search"),
        (Action::ManagerPin, "manager.pin"),
        (Action::ManagerClose, "manager.close"),
    ];

    pub fn from_id(id: &str) -> Option<Action> {
        Self::ALL.iter().find(|(_, i)| *i == id).map(|(a, _)| *a)
    }

    pub fn id(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(a, _)| *a == self)
            .map_or("", |(_, i)| i)
    }
}

/// One key press as the matcher sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Press {
    /// The virtual key (the layout's logical key).
    pub vk: u32,
    /// The scan code (`lParam` bits 16–23) and the extended-key flag (bit 24).
    pub scan: u32,
    pub extended: bool,
    /// The modifiers held, with their side when only one side is down.
    pub mods: HMods,
    /// The character the key produces on the current layout, when a binding needs it.
    pub ch: Option<char>,
    /// An auto-repeat of a key that is held down.
    pub repeat: bool,
}

/// What a key press did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Not a shortcut: the key goes on to the program (or the state screen).
    Ignored,
    /// The first steps of a sequence: waiting for the next one (no timer).
    Pending,
    /// A pending sequence did not continue: the key is swallowed and a hint is shown.
    Cancelled,
    /// A shortcut's key that does nothing now (an auto-repeat, or the press of a `release`
    /// binding): it is swallowed, never sent to the program.
    Swallowed,
    /// A `hold` binding is down: fire it if it is still held after this many milliseconds.
    Hold(u64),
    /// Run this action.
    Fire(Action),
}

/// One active binding.
#[derive(Debug, Clone)]
struct Active {
    action: Action,
    keys: String,
    steps: Vec<Step>,
    scope: Scope,
    trigger: Trigger,
    hold_ms: u64,
    repeat: bool,
}

/// Which surface a matcher serves: the window (its `app` actions and the `terminal` context) or
/// the Terminal manager (the `manager` context, Step 18).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Window,
    Manager,
}

impl Surface {
    /// Whether a binding of `scope` is active on this surface.
    fn takes(self, scope: &Scope) -> bool {
        match self {
            Self::Window => {
                matches!(scope, Scope::App) || *scope == Scope::Context(sh::TERMINAL_CONTEXT.into())
            }
            Self::Manager => *scope == Scope::Context(sh::MANAGER_CONTEXT.into()),
        }
    }

    /// The surface's innermost context (its bindings win over `app` ones, §9.1).
    fn context(self) -> &'static str {
        match self {
            Self::Window => sh::TERMINAL_CONTEXT,
            Self::Manager => sh::MANAGER_CONTEXT,
        }
    }
}

/// The window's shortcuts: Subak's bindings after layering, and the state of a pending sequence
/// or an armed `release`/`hold` binding.
#[derive(Debug)]
pub struct Matcher {
    surface: Surface,
    loaded: Loaded,
    active: Vec<Active>,
    pending: Option<(usize, Vec<usize>)>,
    /// A `release` or `hold` binding whose key is down: (binding, virtual key).
    armed: Option<(usize, u32)>,
    off: Option<&'static str>,
    /// Why `hotkeys.json` is not used (safe mode), or why a reload was ignored.
    pub problem: Option<String>,
}

/// The contract's name of a virtual key (§5.2), if it has one.
pub fn named_key(vk: u32, extended: bool) -> Option<&'static str> {
    let name: String = match vk {
        0x41..=0x5A | 0x30..=0x39 => char::from(vk as u8).to_string(),
        0x70..=0x87 => format!("F{}", vk - 0x6F),
        0x60..=0x69 => format!("Numpad{}", vk - 0x60),
        0x0D if extended => "NumpadEnter".into(),
        _ => match vk {
            0x1B => "Escape",
            0x09 => "Tab",
            0x0D => "Enter",
            0x20 => "Space",
            0x08 => "Backspace",
            0x2D => "Insert",
            0x2E => "Delete",
            0x24 => "Home",
            0x23 => "End",
            0x21 => "PageUp",
            0x22 => "PageDown",
            0x26 => "Up",
            0x28 => "Down",
            0x25 => "Left",
            0x27 => "Right",
            0xBB => "Plus",
            0xBD => "Minus",
            0xBC => "Comma",
            0xBE => "Period",
            0x6A => "NumpadMultiply",
            0x6B => "NumpadAdd",
            0x6D => "NumpadSubtract",
            0x6E => "NumpadDecimal",
            0x6F => "NumpadDivide",
            0x2C => "PrintScreen",
            0x13 => "Pause",
            0x91 => "ScrollLock",
            0x5D => "ContextMenu",
            0xB0 => "MediaNext",
            0xB1 => "MediaPrevious",
            0xB2 => "MediaStop",
            0xB3 => "MediaPlayPause",
            0xAD => "VolumeMute",
            0xAE => "VolumeDown",
            0xAF => "VolumeUp",
            0x15 => "HangulMode",
            0x19 => "Hanja",
            0x1C => "Convert",
            0x1D => "NonConvert",
            _ => return None,
        }
        .into(),
    };
    NAMED_KEYS.iter().find(|n| **n == name).copied()
}

/// The `code:` name of a scan code (set 1, as Windows reports it; §6.1), if it has one.
pub fn code_key(scan: u32, extended: bool) -> Option<&'static str> {
    if extended {
        return None;
    }
    let name = match scan {
        0x1E => "KeyA",
        0x30 => "KeyB",
        0x2E => "KeyC",
        0x20 => "KeyD",
        0x12 => "KeyE",
        0x21 => "KeyF",
        0x22 => "KeyG",
        0x23 => "KeyH",
        0x17 => "KeyI",
        0x24 => "KeyJ",
        0x25 => "KeyK",
        0x26 => "KeyL",
        0x32 => "KeyM",
        0x31 => "KeyN",
        0x18 => "KeyO",
        0x19 => "KeyP",
        0x10 => "KeyQ",
        0x13 => "KeyR",
        0x1F => "KeyS",
        0x14 => "KeyT",
        0x16 => "KeyU",
        0x2F => "KeyV",
        0x11 => "KeyW",
        0x2D => "KeyX",
        0x15 => "KeyY",
        0x2C => "KeyZ",
        0x02 => "Digit1",
        0x03 => "Digit2",
        0x04 => "Digit3",
        0x05 => "Digit4",
        0x06 => "Digit5",
        0x07 => "Digit6",
        0x08 => "Digit7",
        0x09 => "Digit8",
        0x0A => "Digit9",
        0x0B => "Digit0",
        0x0C => "Minus",
        0x0D => "Equal",
        0x1A => "BracketLeft",
        0x1B => "BracketRight",
        0x2B => "Backslash",
        0x27 => "Semicolon",
        0x28 => "Quote",
        0x29 => "Backquote",
        0x33 => "Comma",
        0x34 => "Period",
        0x35 => "Slash",
        0x56 => "IntlBackslash",
        0x73 => "IntlRo",
        0x7D => "IntlYen",
        0x72 => "Lang1",
        0x71 => "Lang2",
        _ => return None,
    };
    CODE_KEYS.iter().find(|n| **n == name).copied()
}

/// Whether a key press can be a binding's step (a `char:` step needs the produced character).
fn hits(p: &Press, s: &Step) -> bool {
    let Step::Chord(want) = s else {
        return false;
    };
    let mut candidates: Vec<Step> = Vec::with_capacity(3);
    if let Some(n) = named_key(p.vk, p.extended) {
        candidates.push(Step::Chord(Chord {
            mods: p.mods,
            key: HKey::Named(n),
        }));
    }
    if let Some(c) = code_key(p.scan, p.extended) {
        candidates.push(Step::Chord(Chord {
            mods: p.mods,
            key: HKey::Code(c),
        }));
    }
    if let (HKey::Char(c), Some(ch)) = (want.key, p.ch) {
        // A `char:` step is whatever produces that character (Shift is implied, §5.5); Ctrl,
        // Alt and Win make it a chord, not a character.
        return c == ch && !p.mods.has_command();
    }
    candidates
        .iter()
        .any(|c| kim_hotkeys::keys::steps_overlap(c, s))
}

impl Matcher {
    /// Subak's shortcuts from `hotkeys.json` in `dir` over the shipped defaults. `off`
    /// (`--no-hotkeys`, `KIM_NO_HOTKEYS`) turns every shortcut off without touching the file.
    pub fn load(dir: Option<&Path>, off: Option<&'static str>) -> Matcher {
        Self::load_for(Surface::Window, dir, off)
    }

    /// The shortcuts of `surface` (the window's, or the Terminal manager's).
    pub fn load_for(surface: Surface, dir: Option<&Path>, off: Option<&'static str>) -> Matcher {
        let (_, loaded) = sh::load(dir);
        let mut m = Matcher {
            surface,
            problem: sh::safe_mode_warning(&loaded),
            loaded,
            active: Vec::new(),
            pending: None,
            armed: None,
            off,
        };
        m.rebuild();
        m
    }

    /// The defaults only (tests, no settings folder).
    #[cfg(test)]
    pub fn defaults() -> Matcher {
        Self::load(None, None)
    }

    fn state(&self) -> ToolState {
        sh::state(sh::manifest(), &self.loaded)
    }

    fn rebuild(&mut self) {
        self.pending = None;
        self.armed = None;
        self.active.clear();
        if self.off.is_some() {
            return;
        }
        let state = self.state();
        for b in analyze(std::slice::from_ref(&state), &[]).active {
            let Some(action) = Action::from_id(&b.action) else {
                continue; // not implemented by this build (never shipped)
            };
            if !self.surface.takes(&b.scope) {
                continue;
            }
            let hold_ms = state
                .manifest
                .action(&b.action)
                .and_then(|d| state.specs(d))
                .and_then(|(specs, _)| {
                    specs.into_iter().find_map(|s| match s {
                        BindingSpec::Object(o) if o.keys == b.keys => o.hold_ms,
                        _ => None,
                    })
                })
                .unwrap_or(500);
            self.active.push(Active {
                action,
                keys: b.keys.clone(),
                steps: b.seq.steps().to_vec(),
                scope: b.scope.clone(),
                trigger: b.trigger,
                hold_ms,
                repeat: b.repeat,
            });
        }
    }

    /// Re-reads `hotkeys.json` (the window regained focus, HOTKEY_CONTRACT §8.3 5). An invalid
    /// file keeps the current bindings and is reported. Returns whether anything changed.
    pub fn reload(&mut self) -> bool {
        let (next, problems) = store::reload(&self.loaded, &sh::manifest());
        let problem = match problems.first() {
            Some(p) => Some(format!(
                "hotkeys.json is invalid ({p}); the previous shortcuts stay active"
            )),
            None => sh::safe_mode_warning(&next),
        };
        let changed = next != self.loaded || problem != self.problem;
        if changed {
            self.problem = problem;
            self.loaded = next;
            self.rebuild();
        }
        changed
    }

    /// Why every shortcut is off for this run, if it is.
    pub fn off(&self) -> Option<&'static str> {
        self.off
    }

    /// The state of the user layer (`default`, `user`, `invalid`, `newer_version`).
    pub fn config_state(&self) -> ConfigState {
        self.loaded.state
    }

    /// Whether any binding needs the produced character of a key press (`char:` steps).
    pub fn wants_chars(&self) -> bool {
        self.active.iter().any(|a| {
            a.steps
                .iter()
                .any(|s| matches!(s, Step::Chord(c) if matches!(c.key, HKey::Char(_))))
        })
    }

    /// Whether a sequence is waiting for its next step.
    #[cfg(test)]
    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// The keys typed so far of a pending sequence (for the hint).
    pub fn pending_keys(&self) -> Option<String> {
        let (step, pool) = self.pending.as_ref()?;
        let first = self.active.get(*pool.first()?)?;
        Some(
            first.steps[..*step]
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" "),
        )
    }

    /// Cancels a pending sequence and an armed binding (`Esc`, a click, focus loss).
    pub fn cancel(&mut self) {
        self.pending = None;
        self.armed = None;
    }

    /// One key press (not a lone modifier).
    pub fn press(&mut self, p: &Press) -> Outcome {
        if self.active.is_empty() {
            return Outcome::Ignored;
        }
        if let Some((i, vk)) = self.armed {
            if p.vk == vk && p.repeat {
                return Outcome::Swallowed; // the armed key auto-repeats
            }
            // Another key in between: a `release` binding does not fire; a `hold` stops.
            let _ = i;
            self.armed = None;
        }
        let (step, pool) = match self.pending.take() {
            Some((step, pool)) => {
                if p.vk == 0x1B && !p.mods.has_command() && !p.mods.shift.on() {
                    return Outcome::Swallowed; // Esc cancels a pending sequence
                }
                (step, pool)
            }
            None => (0, (0..self.active.len()).collect()),
        };
        let matched: Vec<usize> = pool
            .into_iter()
            .filter(|i| self.active[*i].steps.get(step).is_some_and(|s| hits(p, s)))
            .collect();
        // The innermost context wins over `app` bindings with the same keys (§9.1).
        let terminal = Scope::Context(self.surface.context().into());
        let matched = if matched.iter().any(|i| self.active[*i].scope == terminal) {
            matched
                .into_iter()
                .filter(|i| self.active[*i].scope == terminal)
                .collect()
        } else {
            matched
        };
        if matched.is_empty() {
            return if step > 0 {
                Outcome::Cancelled
            } else {
                Outcome::Ignored
            };
        }
        if let Some(&i) = matched
            .iter()
            .find(|i| self.active[**i].steps.len() == step + 1)
        {
            let a = &self.active[i];
            if p.repeat && !a.repeat {
                return Outcome::Swallowed;
            }
            return match a.trigger {
                Trigger::Press => Outcome::Fire(a.action),
                Trigger::Release => {
                    self.armed = Some((i, p.vk));
                    Outcome::Swallowed
                }
                Trigger::Hold => {
                    self.armed = Some((i, p.vk));
                    Outcome::Hold(a.hold_ms)
                }
            };
        }
        if p.repeat && step == 0 {
            return Outcome::Swallowed;
        }
        self.pending = Some((step + 1, matched));
        Outcome::Pending
    }

    /// A key went up: a `release` binding of that key fires; a `hold` binding stops waiting.
    pub fn release(&mut self, vk: u32) -> Option<Action> {
        let (i, armed_vk) = self.armed?;
        if armed_vk != vk {
            return None;
        }
        self.armed = None;
        let a = &self.active[i];
        (a.trigger == Trigger::Release).then_some(a.action)
    }

    /// The hold delay of an armed `hold` binding ended while its key is still down.
    pub fn hold_elapsed(&mut self) -> Option<Action> {
        let (i, _) = self.armed.take()?;
        let a = &self.active[i];
        (a.trigger == Trigger::Hold).then_some(a.action)
    }

    /// The first key text bound to `action` (menus and hints), if any.
    pub fn keys_for(&self, action: Action) -> Option<&str> {
        self.active
            .iter()
            .find(|a| a.action == action)
            .map(|a| a.keys.as_str())
    }

    /// Every active binding as `(action, keys, scope)` (test reports).
    pub fn bindings(&self) -> Vec<(&'static str, String, String)> {
        self.active
            .iter()
            .map(|a| (a.action.id(), a.keys.clone(), a.scope.to_string()))
            .collect()
    }

    /// The file the bindings come from.
    pub fn path(&self) -> PathBuf {
        self.loaded.path.clone()
    }
}

/// The modifiers of a pane key event as the contract's (generic sides).
pub fn contract_mods(m: Mods) -> HMods {
    use kim_hotkeys::keys::Side;
    let on = |b: bool| if b { Side::Any } else { Side::None };
    HMods {
        ctrl: on(m.ctrl),
        alt: on(m.alt),
        altgr: false,
        shift: on(m.shift),
        win: Side::None,
    }
}

/// Virtual-key codes the window reads (documented `VK_*` values).
pub mod vk {
    pub const BACK: u32 = 0x08;
    pub const TAB: u32 = 0x09;
    pub const RETURN: u32 = 0x0D;
    pub const SHIFT: u32 = 0x10;
    pub const CONTROL: u32 = 0x11;
    pub const MENU: u32 = 0x12;
    pub const ESCAPE: u32 = 0x1B;
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
    pub const LWIN: u32 = 0x5B;
    pub const RWIN: u32 = 0x5C;
    pub const APPS: u32 = 0x5D;
    pub const F1: u32 = 0x70;
    pub const F4: u32 = 0x73;
    pub const F10: u32 = 0x79;
    pub const F12: u32 = 0x7B;
    pub const LSHIFT: u32 = 0xA0;
    pub const RSHIFT: u32 = 0xA1;
    pub const LCONTROL: u32 = 0xA2;
    pub const RCONTROL: u32 = 0xA3;
    pub const LMENU: u32 = 0xA4;
    pub const RMENU: u32 = 0xA5;
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
    use kim_hotkeys::keys::Side;
    use serde_json::json;

    fn press(vk: u32, m: Mods) -> Press {
        Press {
            vk,
            scan: 0,
            extended: false,
            mods: contract_mods(m),
            ch: None,
            repeat: false,
        }
    }

    const CS: Mods = Mods {
        ctrl: true,
        shift: true,
        alt: false,
    };

    #[allow(clippy::needless_pass_by_value)]
    fn sandbox(tag: &str, layer: serde_json::Value) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "subak-keys-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(sh::layer_path(&d), layer.to_string()).unwrap();
        d
    }

    #[test]
    fn every_implemented_action_has_an_id_in_the_manifest() {
        let m = sh::manifest();
        assert_eq!(Action::ALL.len(), sh::IMPLEMENTED.len());
        for ((a, id), want) in Action::ALL.iter().zip(sh::IMPLEMENTED) {
            assert_eq!(id, want);
            assert_eq!(Action::from_id(id), Some(*a));
            assert_eq!(a.id(), *id);
            assert!(m.action(id).is_some(), "{id}");
        }
    }

    #[test]
    fn the_requested_tab_keys_and_the_step_16_keys_fire_with_exact_modifiers_only() {
        let mut s = Matcher::defaults();
        let fire = |s: &mut Matcher, vk: u32, m: Mods| s.press(&press(vk, m));
        // Exactly the requested keys (SUBAK-FR-033).
        assert_eq!(
            fire(&mut s, vk::TAB, Mods::CTRL),
            Outcome::Fire(Action::NextTab)
        );
        assert_eq!(
            fire(&mut s, vk::TAB, CS),
            Outcome::Fire(Action::PreviousTab)
        );
        assert_eq!(
            fire(&mut s, u32::from(b'T'), Mods::CTRL),
            Outcome::Fire(Action::NewTerminal)
        );
        assert_eq!(
            fire(&mut s, u32::from(b'W'), Mods::CTRL),
            Outcome::Fire(Action::CloseTerminal)
        );
        assert_eq!(
            fire(&mut s, u32::from(b'R'), CS),
            Outcome::Fire(Action::RenameTerminal)
        );
        // The Step 16 keys keep working (now rebindable).
        assert_eq!(
            fire(&mut s, u32::from(b'C'), CS),
            Outcome::Fire(Action::Copy)
        );
        assert_eq!(
            fire(&mut s, vk::INSERT, Mods::SHIFT),
            Outcome::Fire(Action::Paste)
        );
        assert_eq!(
            fire(&mut s, 0xBB, Mods::CTRL),
            Outcome::Fire(Action::ZoomIn)
        );
        assert_eq!(
            fire(&mut s, vk::PRIOR, CS),
            Outcome::Fire(Action::ScrollPageUp)
        );
        // Keys for the program: Ctrl+C (interrupt), Ctrl+V, Tab, Shift+Tab, Ctrl+B (until the
        // Step 23 prefix), Ctrl+Shift+T, Alt+W, plain letters.
        for (code, m) in [
            (u32::from(b'C'), Mods::CTRL),
            (u32::from(b'V'), Mods::CTRL),
            (vk::TAB, Mods::NONE),
            (vk::TAB, Mods::SHIFT),
            (u32::from(b'B'), Mods::CTRL),
            (u32::from(b'T'), CS),
            (u32::from(b'W'), Mods::ALT),
            (u32::from(b'W'), Mods::NONE),
            (vk::PRIOR, Mods::SHIFT),
            (vk::F1, Mods::NONE),
        ] {
            assert_eq!(fire(&mut s, code, m), Outcome::Ignored, "{code:#x} {m:?}");
        }
        // Holding Ctrl+W never repeats the close and never leaks to the program.
        let mut p = press(u32::from(b'W'), Mods::CTRL);
        p.repeat = true;
        assert_eq!(s.press(&p), Outcome::Swallowed);
        assert!(!s.is_pending());
    }

    #[test]
    fn rebinding_disabling_and_switching_off_follow_hotkeys_json() {
        let dir = sandbox(
            "rebind",
            json!({"schema": "kim.hotkeys", "schema_version": 1, "tool": "subak",
            "overrides": [
                {"action": "terminal.close", "disabled": true},
                {"action": "tab.next", "bindings": ["Ctrl+PageDown"]},
                {"action": "tab.hide", "bindings": ["Ctrl+Shift+W"]},
                {"action": "terminal.new", "bindings": ["Ctrl+K Ctrl+N"]}
            ]}),
        );
        let mut s = Matcher::load(Some(&dir), None);
        assert_eq!(s.config_state(), ConfigState::User);
        // Disabled: Ctrl+W goes to the program (readline's delete-word, SUBAK-FR-034).
        assert_eq!(
            s.press(&press(u32::from(b'W'), Mods::CTRL)),
            Outcome::Ignored
        );
        assert_eq!(s.press(&press(vk::TAB, Mods::CTRL)), Outcome::Ignored);
        assert_eq!(
            s.press(&press(vk::NEXT, Mods::CTRL)),
            Outcome::Fire(Action::NextTab)
        );
        assert_eq!(
            s.press(&press(u32::from(b'W'), CS)),
            Outcome::Fire(Action::HideTab)
        );
        // A sequence: the first step waits, the second fires; a wrong second step is swallowed.
        assert_eq!(
            s.press(&press(u32::from(b'K'), Mods::CTRL)),
            Outcome::Pending
        );
        assert_eq!(s.pending_keys().as_deref(), Some("Ctrl+K"));
        assert_eq!(
            s.press(&press(u32::from(b'N'), Mods::CTRL)),
            Outcome::Fire(Action::NewTerminal)
        );
        assert_eq!(
            s.press(&press(u32::from(b'K'), Mods::CTRL)),
            Outcome::Pending
        );
        assert_eq!(
            s.press(&press(u32::from(b'X'), Mods::NONE)),
            Outcome::Cancelled
        );
        assert!(!s.is_pending());
        assert_eq!(
            s.press(&press(u32::from(b'K'), Mods::CTRL)),
            Outcome::Pending
        );
        assert_eq!(s.press(&press(vk::ESCAPE, Mods::NONE)), Outcome::Swallowed);
        assert!(!s.is_pending());
        // A file that breaks later keeps the bindings in force (§8.3 5).
        std::fs::write(sh::layer_path(&dir), "{ broken").unwrap();
        assert!(s.reload());
        assert!(
            s.problem
                .as_deref()
                .unwrap()
                .contains("previous shortcuts stay")
        );
        assert_eq!(
            s.press(&press(vk::NEXT, Mods::CTRL)),
            Outcome::Fire(Action::NextTab)
        );
        // Starting with a broken file: safe mode, defaults.
        let mut safe = Matcher::load(Some(&dir), None);
        assert_eq!(safe.config_state(), ConfigState::Invalid);
        assert!(safe.problem.is_some());
        assert_eq!(
            safe.press(&press(u32::from(b'W'), Mods::CTRL)),
            Outcome::Fire(Action::CloseTerminal)
        );
        // --no-hotkeys / KIM_NO_HOTKEYS: nothing is a shortcut, every key reaches the program.
        let mut off = Matcher::load(Some(&dir), Some("--no-hotkeys"));
        assert_eq!(off.off(), Some("--no-hotkeys"));
        assert_eq!(off.press(&press(vk::TAB, Mods::CTRL)), Outcome::Ignored);
        assert_eq!(
            off.press(&press(u32::from(b'C'), CS)),
            Outcome::Ignored,
            "copy is off too"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn physical_character_release_and_hold_bindings() {
        let dir = sandbox(
            "kinds",
            json!({"schema": "kim.hotkeys", "schema_version": 1, "tool": "subak",
            "overrides": [
                {"action": "tab.move-left", "bindings": ["Ctrl+Shift+code:KeyQ"]},
                {"action": "tab.move-right", "bindings": ["Ctrl+K char:?"]},
                {"action": "tab.hide", "bindings": [{"keys": "F9", "trigger": "release"}]},
                {"action": "terminal.rename", "bindings": [{"keys": "F8", "trigger": "hold", "hold_ms": 700}]},
                {"action": "tab.next", "bindings": [{"keys": "Ctrl+Tab", "repeat": true}]}
            ]}),
        );
        let mut s = Matcher::load(Some(&dir), None);
        assert!(s.wants_chars());
        // `code:KeyQ` is the key in Q's position whatever the layout says (AZERTY: A).
        let mut p = press(u32::from(b'A'), CS);
        p.scan = 0x10;
        assert_eq!(s.press(&p), Outcome::Fire(Action::MoveTabLeft));
        // `char:?` after Ctrl+K: whatever produces `?` on this layout.
        assert_eq!(
            s.press(&press(u32::from(b'K'), Mods::CTRL)),
            Outcome::Pending
        );
        let mut q = press(0xBF, Mods::SHIFT);
        q.ch = Some('?');
        assert_eq!(s.press(&q), Outcome::Fire(Action::MoveTabRight));
        // `release`: fires when the key goes up with nothing pressed in between.
        assert_eq!(s.press(&press(0x78, Mods::NONE)), Outcome::Swallowed);
        assert_eq!(s.release(0x78), Some(Action::HideTab));
        assert_eq!(s.press(&press(0x78, Mods::NONE)), Outcome::Swallowed);
        assert_eq!(
            s.press(&press(u32::from(b'X'), Mods::NONE)),
            Outcome::Ignored
        );
        assert_eq!(s.release(0x78), None, "another key in between");
        // `hold`: the window waits hold_ms; an earlier release does nothing.
        assert_eq!(s.press(&press(0x77, Mods::NONE)), Outcome::Hold(700));
        assert_eq!(s.release(0x77), None);
        assert_eq!(s.hold_elapsed(), None);
        assert_eq!(s.press(&press(0x77, Mods::NONE)), Outcome::Hold(700));
        assert_eq!(s.hold_elapsed(), Some(Action::RenameTerminal));
        // `repeat: true` lets auto-repeat fire again.
        let mut r = press(vk::TAB, Mods::CTRL);
        r.repeat = true;
        assert_eq!(s.press(&r), Outcome::Fire(Action::NextTab));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn key_names_follow_the_contract() {
        assert_eq!(named_key(u32::from(b'T'), false), Some("T"));
        assert_eq!(named_key(0x7B, false), Some("F12"));
        assert_eq!(named_key(0x0D, true), Some("NumpadEnter"));
        assert_eq!(named_key(0x0D, false), Some("Enter"));
        assert_eq!(named_key(vk::TAB, false), Some("Tab"));
        assert_eq!(
            named_key(0xBF, false),
            None,
            "VK_OEM_2 has no layout-stable name"
        );
        assert_eq!(code_key(0x10, false), Some("KeyQ"));
        assert_eq!(code_key(0x0B, false), Some("Digit0"));
        assert_eq!(code_key(0x48, true), None);
        // Side-specific bindings need that side.
        let dir = sandbox(
            "side",
            json!({"schema": "kim.hotkeys", "schema_version": 1, "tool": "subak",
                "overrides": [{"action": "tab.hide", "bindings": ["RCtrl+H"]}]}),
        );
        let mut s = Matcher::load(Some(&dir), None);
        let mut p = press(u32::from(b'H'), Mods::CTRL);
        p.mods.ctrl = Side::Left;
        assert_eq!(s.press(&p), Outcome::Ignored);
        p.mods.ctrl = Side::Right;
        assert_eq!(s.press(&p), Outcome::Fire(Action::HideTab));
        let _ = std::fs::remove_dir_all(&dir);
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
