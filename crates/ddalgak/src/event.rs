//! The normalized keyboard event model (ADR 0024 §2, docs/DDALGAK_ENGINE.md §2).
//!
//! A low-level keyboard hook reports one [`RawKey`] per key transition: virtual key, scan code,
//! flags and `dwExtraInfo`. [`Normalizer`] turns it into a [`KeyEvent`]:
//!
//! - **Logical and physical identity.** Both the virtual key (which the active layout assigns)
//!   and the scan code (the position) are kept; triggers match whichever they name
//!   (HOTKEY_CONTRACT §6.1). No character translation happens here: that would disturb the
//!   focused application's dead-key state.
//! - **Left and right modifiers.** Generic modifier codes are resolved to sides; the fake left
//!   Ctrl that Windows sends before Right Alt on AltGr layouts (scan `0x21D`) is recognized, so
//!   `AltGr+Q` and `Ctrl+Alt+Q` stay distinct.
//! - **Down, repeat and up.** Auto-repeat is a down for a key that is already down. Holds are
//!   decided by the engine core from the down time ([`crate::core`]).
//! - **Source.** Physical input, Ddalgak's own injected input (`LLKHF_INJECTED` plus
//!   [`OWN_MARKER`] in `dwExtraInfo`) and other programs' injected input are told apart, so the
//!   engine never re-processes what it injected (`DDALGAK-SEC-004`). A low-level hook does not
//!   say which keyboard a key came from; the device is therefore `unknown` for physical keys
//!   (ADR 0024 §2).
//!
//! The model keeps only what matching needs: which keys and modifiers are down. Nothing is
//! recorded, logged or kept after the key is released (`DDALGAK-SEC-001`).

use crate::vk::{self, ModKey};

/// Ddalgak's tag in `dwExtraInfo` of every event it injects ("DDLG"). Combined with
/// `LLKHF_INJECTED`, which only Windows sets, it marks Ddalgak's own input. Another program could
/// copy the tag, but that only hides its input from Ddalgak's rules; it cannot make input look
/// physical, so it cannot trigger or suppress the emergency chord.
pub const OWN_MARKER: usize = 0x4444_4C47;

/// One key transition as a low-level keyboard hook reports it (`KBDLLHOOKSTRUCT`), as plain
/// values so the engine core is platform independent and testable.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct RawKey {
    pub vk: u16,
    pub scan: u16,
    /// `LLKHF_EXTENDED`.
    pub extended: bool,
    /// `LLKHF_UP`.
    pub up: bool,
    /// `LLKHF_INJECTED`.
    pub injected: bool,
    /// `LLKHF_LOWER_IL_INJECTED`: injected by a lower-integrity process.
    pub lower_il_injected: bool,
    /// `dwExtraInfo`.
    pub extra_info: usize,
    /// Event time in milliseconds (`GetTickCount` base).
    pub time_ms: u32,
}

impl RawKey {
    /// A physical key event (tests and the playground build events this way).
    pub fn physical(vk: u16, scan: u16, up: bool, time_ms: u32) -> RawKey {
        RawKey {
            vk,
            scan,
            up,
            time_ms,
            ..RawKey::default()
        }
    }
}

/// Where a key event came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    /// A keyboard (no `LLKHF_INJECTED`).
    Physical,
    /// Injected by Ddalgak itself: passed on, never matched.
    Own,
    /// Injected by another program (`SendInput`, an on-screen keyboard, remote control).
    Other,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Physical => "physical",
            Source::Own => "injected_by_ddalgak",
            Source::Other => "injected_by_other_program",
        }
    }
}

/// The source device of a key, where the platform reports it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Device {
    /// `WH_KEYBOARD_LL` does not identify the keyboard (ADR 0024 §2).
    Unknown,
    /// Synthesized input has no device.
    Synthetic,
}

/// Down, auto-repeat or up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Transition {
    Down,
    Repeat,
    Up,
}

/// A normalized key event.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KeyEvent {
    /// Virtual key, side-specific for modifiers.
    pub vk: u16,
    /// Scan code without the extended bit.
    pub scan: u16,
    pub extended: bool,
    pub transition: Transition,
    pub source: Source,
    pub device: Device,
    /// The modifier this key is, if any (the fake AltGr Ctrl is not one).
    pub modifier: Option<ModKey>,
    /// The fake left Ctrl of an AltGr layout.
    pub altgr_fake_ctrl: bool,
    pub time_ms: u32,
}

/// A 256-bit set of virtual keys.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct KeySet([u64; 4]);

impl KeySet {
    /// An empty set (usable in a `static`).
    pub const fn empty() -> KeySet {
        KeySet([0; 4])
    }

    pub fn contains(&self, vk: u16) -> bool {
        let i = usize::from(vk & 0xFF);
        self.0[i / 64] & (1 << (i % 64)) != 0
    }

    pub fn insert(&mut self, vk: u16) -> bool {
        let i = usize::from(vk & 0xFF);
        let had = self.contains(vk);
        self.0[i / 64] |= 1 << (i % 64);
        !had
    }

    pub fn remove(&mut self, vk: u16) -> bool {
        let i = usize::from(vk & 0xFF);
        let had = self.contains(vk);
        self.0[i / 64] &= !(1 << (i % 64));
        had
    }

    pub fn is_empty(&self) -> bool {
        self.0 == [0; 4]
    }

    pub fn clear(&mut self) {
        self.0 = [0; 4];
    }

    pub fn len(&self) -> usize {
        self.0.iter().map(|w| w.count_ones() as usize).sum()
    }

    /// The keys in the set, in ascending order.
    pub fn iter(&self) -> impl Iterator<Item = u16> + '_ {
        (0u16..256).filter(|v| self.contains(*v))
    }
}

/// Which modifier keys are down, with the AltGr state.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ModState {
    bits: u8,
    /// The fake AltGr Ctrl is down.
    fake_ctrl: bool,
}

impl ModState {
    pub fn has(&self, m: ModKey) -> bool {
        self.bits & m.bit() != 0
    }

    fn set(&mut self, m: ModKey, down: bool) {
        if down {
            self.bits |= m.bit();
        } else {
            self.bits &= !m.bit();
        }
    }

    /// AltGr is held: the fake Ctrl together with Right Alt.
    pub fn altgr(&self) -> bool {
        self.fake_ctrl && self.has(ModKey::RAlt)
    }

    pub fn ctrl(&self) -> bool {
        self.has(ModKey::LCtrl) || self.has(ModKey::RCtrl)
    }

    /// Alt that is not part of AltGr.
    pub fn alt(&self) -> bool {
        self.has(ModKey::LAlt) || (self.has(ModKey::RAlt) && !self.altgr())
    }

    pub fn shift(&self) -> bool {
        self.has(ModKey::LShift) || self.has(ModKey::RShift)
    }

    pub fn win(&self) -> bool {
        self.has(ModKey::LWin) || self.has(ModKey::RWin)
    }

    pub fn is_empty(&self) -> bool {
        self.bits == 0 && !self.fake_ctrl
    }

    /// Ctrl, Alt and Shift for the emergency chord: AltGr's fake Ctrl counts as Ctrl there, so
    /// the chord also works with AltGr on layouts that have it (recovery must stay possible).
    pub fn emergency_mods(&self) -> bool {
        (self.ctrl() || self.fake_ctrl)
            && (self.has(ModKey::LAlt) || self.has(ModKey::RAlt))
            && self.shift()
            && !self.win()
    }

    /// The state as the contract's modifier set (for reserved-chord checks).
    pub fn as_mods(&self) -> kim_hotkeys::keys::Mods {
        use kim_hotkeys::keys::{Mods, Side};
        let side = |l: bool, r: bool| match (l, r) {
            (false, false) => Side::None,
            (true, false) => Side::Left,
            (false, true) => Side::Right,
            (true, true) => Side::Any,
        };
        let altgr = self.altgr();
        Mods {
            ctrl: side(self.has(ModKey::LCtrl), self.has(ModKey::RCtrl)),
            alt: side(self.has(ModKey::LAlt), self.has(ModKey::RAlt) && !altgr),
            altgr,
            shift: side(self.has(ModKey::LShift), self.has(ModKey::RShift)),
            win: side(self.has(ModKey::LWin), self.has(ModKey::RWin)),
        }
    }

    /// Whether a trigger's modifiers are exactly what is held (HOTKEY_CONTRACT §5.5): generic
    /// modifiers accept either side, side-specific ones need that side, nothing else may be
    /// down, and `Win` never is part of a trigger (ADR 0022).
    pub fn matches(&self, req: &kim_hotkeys::keys::Mods) -> bool {
        use kim_hotkeys::keys::Side;
        fn ok(req: Side, l: bool, r: bool) -> bool {
            match req {
                Side::None => !l && !r,
                Side::Any => l || r,
                Side::Left => l,
                Side::Right => r,
            }
        }
        let altgr = self.altgr();
        req.altgr == altgr
            && ok(req.ctrl, self.has(ModKey::LCtrl), self.has(ModKey::RCtrl))
            && ok(
                req.alt,
                self.has(ModKey::LAlt),
                self.has(ModKey::RAlt) && !altgr,
            )
            && ok(
                req.shift,
                self.has(ModKey::LShift),
                self.has(ModKey::RShift),
            )
            && !self.win()
            && req.win == Side::None
    }

    /// Like [`matches`](Self::matches), ignoring one modifier key (a bare-modifier trigger's own
    /// key, which is down while it is the trigger).
    pub fn matches_without(&self, req: &kim_hotkeys::keys::Mods, own: ModKey) -> bool {
        let mut s = *self;
        s.set(own, false);
        s.matches(req)
    }
}

/// Keeps the key state needed to normalize events. Holds no history.
#[derive(Clone, Debug, Default)]
pub struct Normalizer {
    /// Keys down by any source (what Windows considers down).
    down: KeySet,
    /// Keys down physically.
    down_physical: KeySet,
    mods: ModState,
    mods_physical: ModState,
}

impl Normalizer {
    pub fn new() -> Normalizer {
        Normalizer::default()
    }

    /// Classifies and normalizes one event and updates the key state. Allocation-free.
    pub fn normalize(&mut self, raw: &RawKey) -> KeyEvent {
        let source = match (raw.injected, raw.extra_info == OWN_MARKER) {
            (false, _) => Source::Physical,
            (true, true) => Source::Own,
            (true, false) => Source::Other,
        };
        let altgr_fake_ctrl = matches!(raw.vk, vk::VK_LCONTROL | vk::VK_CONTROL)
            && raw.scan == vk::SCAN_ALTGR_FAKE_CTRL;
        let modifier = if altgr_fake_ctrl {
            None
        } else {
            ModKey::from_event(raw.vk, raw.scan, raw.extended)
        };
        let vk = modifier.map_or(raw.vk, ModKey::vk);
        let transition = if raw.up {
            self.down.remove(vk);
            if source == Source::Physical {
                self.down_physical.remove(vk);
            }
            Transition::Up
        } else {
            if source == Source::Physical {
                self.down_physical.insert(vk);
            }
            if self.down.insert(vk) {
                Transition::Down
            } else {
                Transition::Repeat
            }
        };
        let down = !raw.up;
        if altgr_fake_ctrl {
            self.mods.fake_ctrl = down;
            if source == Source::Physical {
                self.mods_physical.fake_ctrl = down;
            }
        } else if let Some(m) = modifier {
            self.mods.set(m, down);
            if source == Source::Physical {
                self.mods_physical.set(m, down);
            }
        }
        KeyEvent {
            vk,
            scan: raw.scan & 0xFF,
            extended: raw.extended,
            transition,
            source,
            device: if raw.injected {
                Device::Synthetic
            } else {
                Device::Unknown
            },
            modifier,
            altgr_fake_ctrl,
            time_ms: raw.time_ms,
        }
    }

    /// Modifiers held by any source.
    pub fn mods(&self) -> ModState {
        self.mods
    }

    /// Modifiers held physically (the emergency chord uses only these).
    pub fn mods_physical(&self) -> ModState {
        self.mods_physical
    }

    pub fn is_down(&self, vk: u16) -> bool {
        self.down.contains(vk)
    }

    pub fn is_down_physical(&self, vk: u16) -> bool {
        self.down_physical.contains(vk)
    }

    /// Forgets all key state (engine start, session unlock): Windows may have delivered events
    /// the hook never saw.
    pub fn reset(&mut self) {
        *self = Normalizer::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vk::*;

    fn ev(n: &mut Normalizer, vk: u16, scan: u16, up: bool) -> KeyEvent {
        n.normalize(&RawKey::physical(vk, scan, up, 0))
    }

    #[test]
    fn down_repeat_up_are_derived_from_key_state() {
        let mut n = Normalizer::new();
        let a = u16::from(b'A');
        assert_eq!(ev(&mut n, a, 0x1E, false).transition, Transition::Down);
        assert_eq!(ev(&mut n, a, 0x1E, false).transition, Transition::Repeat);
        assert_eq!(ev(&mut n, a, 0x1E, false).transition, Transition::Repeat);
        assert_eq!(ev(&mut n, a, 0x1E, true).transition, Transition::Up);
        assert!(!n.is_down(a));
        assert_eq!(ev(&mut n, a, 0x1E, false).transition, Transition::Down);
    }

    #[test]
    fn left_and_right_modifiers_are_kept_apart() {
        let mut n = Normalizer::new();
        let e = ev(&mut n, VK_RCONTROL, 0x1D, false);
        assert_eq!(e.modifier, Some(ModKey::RCtrl));
        assert!(n.mods().has(ModKey::RCtrl) && !n.mods().has(ModKey::LCtrl));
        // Injected input with generic codes is resolved by scan code / extended flag.
        let r = n.normalize(&RawKey {
            vk: VK_SHIFT,
            scan: 0x36,
            injected: true,
            ..RawKey::default()
        });
        assert_eq!(r.vk, VK_RSHIFT);
        assert!(n.mods().has(ModKey::RShift));
        assert!(
            !n.mods_physical().has(ModKey::RShift),
            "injected is not physical"
        );
        use kim_hotkeys::keys::Chord;
        let req = |s: &str| Chord::parse(&format!("{s}F13")).unwrap().mods;
        assert!(n.mods().matches(&req("Ctrl+Shift+")));
        assert!(n.mods().matches(&req("RCtrl+RShift+")));
        assert!(!n.mods().matches(&req("LCtrl+Shift+")));
        assert!(
            !n.mods().matches(&req("Ctrl+")),
            "Shift is also down: not exact"
        );
        ev(&mut n, VK_LWIN, 0x5B, false);
        assert!(
            !n.mods().matches(&req("Ctrl+Shift+")),
            "Win is never part of a trigger"
        );
    }

    #[test]
    fn altgr_is_not_ctrl_alt() {
        use kim_hotkeys::keys::Chord;
        let mut n = Normalizer::new();
        // German layout: AltGr = fake LCtrl (scan 0x21D) + RAlt.
        let fake = ev(&mut n, VK_LCONTROL, SCAN_ALTGR_FAKE_CTRL, false);
        assert!(fake.altgr_fake_ctrl && fake.modifier.is_none());
        ev(&mut n, VK_RMENU, 0x38, false);
        assert!(n.mods().altgr());
        let req = |s: &str| Chord::parse(&format!("{s}Q")).unwrap().mods;
        assert!(n.mods().matches(&req("AltGr+")));
        assert!(!n.mods().matches(&req("Ctrl+Alt+")));
        assert!(!n.mods().matches(&req("RAlt+")));
        // US layout: Right Alt alone is RAlt.
        let mut us = Normalizer::new();
        ev(&mut us, VK_RMENU, 0x38, false);
        assert!(us.mods().matches(&req("RAlt+")));
        assert!(!us.mods().matches(&req("AltGr+")));
    }

    #[test]
    fn sources_are_classified_by_flag_and_marker() {
        let mut n = Normalizer::new();
        let mut raw = RawKey::physical(VK_F13, 0x64, false, 0);
        assert_eq!(n.normalize(&raw).source, Source::Physical);
        assert_eq!(n.normalize(&raw).device, Device::Unknown);
        raw.injected = true;
        assert_eq!(n.normalize(&raw).source, Source::Other);
        raw.extra_info = OWN_MARKER;
        let own = n.normalize(&raw);
        assert_eq!(own.source, Source::Own);
        assert_eq!(own.device, Device::Synthetic);
        // The marker without LLKHF_INJECTED is not Ddalgak's: only Windows sets the flag.
        raw.injected = false;
        assert_eq!(n.normalize(&raw).source, Source::Physical);
    }

    #[test]
    fn emergency_modifiers_use_physical_state_and_accept_altgr() {
        let mut n = Normalizer::new();
        for (vk, scan) in [(VK_LCONTROL, 0x1D), (VK_LMENU, 0x38), (VK_LSHIFT, 0x2A)] {
            n.normalize(&RawKey {
                vk,
                scan,
                injected: true,
                ..RawKey::default()
            });
        }
        assert!(n.mods().emergency_mods());
        assert!(
            !n.mods_physical().emergency_mods(),
            "injected modifiers never count"
        );
        let mut g = Normalizer::new();
        ev(&mut g, VK_LCONTROL, SCAN_ALTGR_FAKE_CTRL, false);
        ev(&mut g, VK_RMENU, 0x38, false);
        ev(&mut g, VK_RSHIFT, 0x36, false);
        assert!(g.mods_physical().emergency_mods(), "AltGr+Shift counts");
    }

    #[test]
    fn same_physical_key_differs_by_layout_and_both_identities_are_kept() {
        // The key at scan 0x2C is Z on US QWERTY and Y on German QWERTZ: Windows reports the
        // virtual key of the active layout and the same scan code.
        let mut n = Normalizer::new();
        let us = ev(&mut n, u16::from(b'Z'), 0x2C, false);
        ev(&mut n, u16::from(b'Z'), 0x2C, true);
        let de = ev(&mut n, u16::from(b'Y'), 0x2C, false);
        assert_eq!(us.scan, de.scan);
        assert_ne!(us.vk, de.vk);
        assert_eq!(code_scan("KeyZ").unwrap().0, us.scan);
    }

    #[test]
    fn keyset_basics() {
        let mut s = KeySet::default();
        assert!(s.insert(0xFF) && s.insert(0) && !s.insert(0));
        assert_eq!(s.len(), 2);
        assert_eq!(s.iter().collect::<Vec<_>>(), vec![0, 0xFF]);
        assert!(s.remove(0) && !s.remove(0));
        s.clear();
        assert!(s.is_empty());
    }
}
