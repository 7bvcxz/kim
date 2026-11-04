//! Recovery (Step 73, ADR 0046; DDALGAK_SAFETY §4, §7, §10; `DDALGAK-SEC-003`,
//! `DDALGAK-SEC-005`, `DDALGAK-NFR-002`).
//!
//! Two portable pieces the Windows host uses:
//!
//! - **Stale keys.** A process that is killed while it holds an injected key down cannot release
//!   it: Windows keeps that key "down" until something sends its up (the user's next press of
//!   the key does). After an unclean exit, and when the user asks (`ddalgak release`, the tray's
//!   *Release stuck keys*), the host reads which of the keys Ddalgak could have pressed
//!   ([`candidates`]: the eight modifiers and every remap or held-button output of the active
//!   rules) Windows reports down, and sends an up for each one nobody holds ([`stale`]): not one
//!   in Ddalgak's own ledger (the worker releases those), not a modifier the hook saw physically
//!   pressed. A key the user really holds and the hook cannot know of (the engine is off) gets an
//!   up too; its auto-repeat presses it again at once. Alt and Win get a masking key first, so
//!   their up opens no menu ([`release_events`]). The debug test host releases only `F13`–`F24`
//!   ([`Scope::ObscureOnly`]): automated tests never send modifier or mouse input.
//! - **The hook timeout.** Windows passes an event on when a low-level hook does not answer within
//!   `LowLevelHooksTimeout` and, from Windows 7 on, removes the hook *silently* (*documented*).
//!   The hook callback measures itself and the queueing delay of physical events; one that
//!   reaches the threshold ([`hook_timeout_ms`]) makes the engine treat the hook as lost: it turns
//!   itself off (`hook_lost`), releases everything and says so. It never reinstalls a hook by
//!   itself; turning the engine on again installs a fresh one.

use crate::inject::{Button, OutCode, OutEvent};
use crate::table::Binding;
use crate::vk::ModKey;

/// `LowLevelHooksTimeout` is capped at this on Windows 10 1709 and later (*documented*).
pub const MAX_HOOK_TIMEOUT_MS: u32 = 1000;
/// What Ddalgak assumes when the registry value is absent (the hook documentation does not state
/// Windows' default): a callback or a wait this long is far beyond the 1 ms budget anyway.
pub const ASSUMED_HOOK_TIMEOUT_MS: u32 = 300;

/// The threshold at which the engine treats its hook as removed, from the user's
/// `HKCU\Control Panel\Desktop\LowLevelHooksTimeout` (`None`: absent or unreadable).
pub fn hook_timeout_ms(registry: Option<u32>) -> u32 {
    match registry {
        Some(v) if v > 0 => v.min(MAX_HOOK_TIMEOUT_MS),
        _ => ASSUMED_HOOK_TIMEOUT_MS,
    }
}

/// The registry value as text (`REG_SZ`, the usual type) in milliseconds.
pub fn parse_timeout(text: &str) -> Option<u32> {
    text.trim().trim_end_matches('\0').trim().parse().ok()
}

/// Whether one callback means the hook may be gone: it ran `callback_us` itself, or a physical
/// event waited `delay_ms` before it reached the callback (an injected event's time is set by
/// its sender and proves nothing). `threshold_us` 0 turns the check off.
pub fn hook_lost(callback_us: u64, delay_ms: u64, physical: bool, threshold_us: u64) -> bool {
    threshold_us > 0
        && (callback_us >= threshold_us
            || (physical && delay_ms.saturating_mul(1000) >= threshold_us))
}

/// Which keys a release may touch.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    /// Every candidate (the shipped app).
    All,
    /// `F13`–`F24` only (the debug test host: the maintainer's real modifiers and mouse buttons
    /// are never touched by an automated test).
    ObscureOnly,
}

const VK_F13: u16 = 0x7C;
const VK_F24: u16 = 0x87;

/// Whether a release in `scope` may touch `code`.
pub fn in_scope(code: OutCode, scope: Scope) -> bool {
    match scope {
        Scope::All => true,
        Scope::ObscureOnly => code.vk().is_some_and(|v| (VK_F13..=VK_F24).contains(&v)),
    }
}

/// The virtual key Windows reports the state of (`GetAsyncKeyState`), for an output that has
/// one: keys by virtual key, and mouse buttons (`VK_*BUTTON`). Scan-code outputs (`code:` keys)
/// have no fixed virtual key and are not checked.
pub fn query_vk(code: OutCode) -> Option<u16> {
    match code {
        OutCode::Button(b) => Some(b.vk()),
        c => c.vk(),
    }
}

/// The keys and buttons Ddalgak could have left pressed: the eight modifiers (remaps hold them,
/// typing runs press Ctrl and Shift again) and every output a remap or a held button of
/// `bindings` holds. Each once, modifiers first.
pub fn candidates(bindings: &[Binding]) -> Vec<OutCode> {
    let mut out: Vec<OutCode> = ModKey::ALL.iter().map(|m| OutCode::Vk(m.vk())).collect();
    for b in bindings {
        let Some(spec) = b.action.remap_spec() else {
            continue;
        };
        for o in [Some(spec.to), spec.tap].into_iter().flatten() {
            if let Some(k) = o.key
                && k.holds()
                && query_vk(k).is_some()
                && !out.contains(&k)
            {
                out.push(k);
            }
        }
    }
    out
}

/// What a release must let go of: the candidates Windows reports down (`is_down`) that nobody
/// holds — not in Ddalgak's `ledger` (the worker releases those itself) and, when the hook knows
/// the physical modifier state (`physical`, [`ModKey::bit`] bits), not a modifier the user is
/// pressing.
pub fn stale(
    candidates: &[OutCode],
    is_down: &dyn Fn(u16) -> bool,
    physical: Option<u8>,
    ledger: &[OutCode],
    scope: Scope,
) -> Vec<OutCode> {
    candidates
        .iter()
        .copied()
        .filter(|c| in_scope(*c, scope))
        .filter(|c| !ledger.contains(c))
        .filter(|c| {
            let held_by_user = physical.is_some_and(|bits| {
                ModKey::ALL
                    .iter()
                    .any(|m| c.vk() == Some(m.vk()) && bits & m.bit() != 0)
            });
            !held_by_user
        })
        .filter(|c| query_vk(*c).is_some_and(is_down))
        .collect()
}

/// The ups for `stale`, in one call: a masking key's tap first when an Alt or a Win key is among
/// them (an up after a lone down of those opens the menu bar or the Start menu), then the ups,
/// buttons last.
pub fn release_events(stale: &[OutCode]) -> Vec<OutEvent> {
    let menu_keys = [ModKey::LAlt, ModKey::RAlt, ModKey::LWin, ModKey::RWin];
    let mut out = Vec::new();
    if stale
        .iter()
        .any(|c| menu_keys.iter().any(|m| c.vk() == Some(m.vk())))
    {
        out.push(OutEvent::down(OutCode::Vk(crate::run::MASK_VK)));
        out.push(OutEvent::up(OutCode::Vk(crate::run::MASK_VK)));
    }
    let (buttons, keys): (Vec<OutCode>, Vec<OutCode>) =
        stale.iter().partition(|c| matches!(c, OutCode::Button(_)));
    out.extend(keys.into_iter().map(OutEvent::up));
    out.extend(buttons.into_iter().map(OutEvent::up));
    out
}

/// A button's name for diagnostics (never keys the user typed: these are rule outputs).
pub fn is_button(code: OutCode) -> Option<Button> {
    match code {
        OutCode::Button(b) => Some(b),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remap::Output;
    use crate::table::{Action, Injected, On, RemapSpec};
    use crate::trigger::Trigger;

    fn remap(trigger: &str, to: &str, tap: Option<&str>) -> Binding {
        Binding {
            rule: format!("r-{trigger}"),
            trigger: Trigger::parse(trigger).unwrap(),
            on: On::Press,
            repeat: false,
            pass_through: false,
            injected: Injected::Ignore,
            widen: true,
            injects: true,
            action: Action::Remap(RemapSpec {
                to: Output::parse(to).unwrap(),
                tap: tap.map(|t| Output::parse(t).unwrap()),
                tap_ms: 200,
            }),
            hotstring: None,
            modes: 0,
        }
    }

    #[test]
    fn the_hook_timeout_follows_the_registry_capped_and_assumed() {
        assert_eq!(hook_timeout_ms(None), ASSUMED_HOOK_TIMEOUT_MS);
        assert_eq!(hook_timeout_ms(Some(0)), ASSUMED_HOOK_TIMEOUT_MS);
        assert_eq!(hook_timeout_ms(Some(200)), 200);
        assert_eq!(
            hook_timeout_ms(Some(5000)),
            1000,
            "Windows 10 1709+ caps it at 1 s"
        );
        assert_eq!(parse_timeout(" 2000\0"), Some(2000));
        assert_eq!(parse_timeout("soon"), None);
        // A slow callback, or a physical event that waited too long, means the hook may be gone.
        assert!(!hook_lost(85, 3, true, 300_000));
        assert!(hook_lost(300_000, 0, false, 300_000));
        assert!(hook_lost(10, 300, true, 300_000));
        assert!(
            !hook_lost(10, 5000, false, 300_000),
            "an injected event's time is its sender's and proves nothing"
        );
        assert!(
            !hook_lost(u64::MAX, u64::MAX, true, 0),
            "0 turns the check off"
        );
    }

    #[test]
    fn candidates_are_the_modifiers_and_every_held_output() {
        let c = candidates(&[
            remap("CapsLock", "LCtrl", Some("Escape")),
            remap("F15", "F16", None),
            remap("F17", "F16", None),
            remap("MouseX1", "Alt+Left", None),
            remap("F18", "code:KeyA", None),
            remap("F19", "MouseMiddle", None),
        ]);
        assert_eq!(&c[..8], &ModKey::ALL.map(|m| OutCode::Vk(m.vk()))[..]);
        let rest: Vec<OutCode> = c[8..].to_vec();
        assert!(rest.contains(&OutCode::Vk(0x1B)), "the tap output (Escape)");
        assert_eq!(
            rest.iter().filter(|x| **x == OutCode::Vk(0x7F)).count(),
            1,
            "F16 once"
        );
        assert!(rest.contains(&OutCode::Button(Button::Middle)));
        assert!(
            !rest.iter().any(|x| matches!(x, OutCode::Scan(_))),
            "scan-code outputs have no virtual key to check: {rest:?}"
        );
    }

    #[test]
    fn only_keys_nobody_holds_are_released_and_tests_touch_only_obscure_keys() {
        let lctrl = OutCode::Vk(ModKey::LCtrl.vk());
        let lshift = OutCode::Vk(ModKey::LShift.vk());
        let f16 = OutCode::Vk(0x7F);
        let f20 = OutCode::Vk(0x83);
        let mid = OutCode::Button(Button::Middle);
        let all = [lctrl, lshift, f16, f20, mid];
        let down =
            |vk: u16| [ModKey::LCtrl.vk(), ModKey::LShift.vk(), 0x7F, 0x83, 0x04].contains(&vk);
        // Engine off: the hook knows nothing physical; the ledger holds F20.
        assert_eq!(
            stale(&all, &down, None, &[f20], Scope::All),
            vec![lctrl, lshift, f16, mid]
        );
        // The hook saw LShift go down physically: the user holds it.
        assert_eq!(
            stale(&all, &down, Some(ModKey::LShift.bit()), &[], Scope::All),
            vec![lctrl, f16, f20, mid]
        );
        // Keys Windows reports up are never touched.
        assert!(stale(&all, &|_| false, None, &[], Scope::All).is_empty());
        // The test host: F13–F24 only, never a modifier or a button.
        assert_eq!(
            stale(&all, &down, None, &[], Scope::ObscureOnly),
            vec![f16, f20]
        );
    }

    #[test]
    fn alt_and_win_get_a_masking_key_first_and_buttons_go_last() {
        let lalt = OutCode::Vk(ModKey::LAlt.vk());
        let mid = OutCode::Button(Button::Middle);
        let e = release_events(&[mid, lalt]);
        assert_eq!(
            e,
            vec![
                OutEvent::down(OutCode::Vk(crate::run::MASK_VK)),
                OutEvent::up(OutCode::Vk(crate::run::MASK_VK)),
                OutEvent::up(lalt),
                OutEvent::up(mid),
            ]
        );
        let f16 = OutCode::Vk(0x7F);
        assert_eq!(release_events(&[f16]), vec![OutEvent::up(f16)]);
        assert!(release_events(&[]).is_empty());
        assert_eq!(is_button(mid), Some(Button::Middle));
    }
}
