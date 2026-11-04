//! Windows side of recovery (Step 73, ADR 0046; portable logic in [`crate::recovery`]).
//!
//! - [`hook_timeout_ms`] reads the user's `HKCU\Control Panel\Desktop\LowLevelHooksTimeout`
//!   once, when a hook is installed (never in the callback, never on a timer).
//! - [`release_stale`] reads `GetAsyncKeyState` of the candidate keys once, when asked (after an
//!   unclean exit, `ddalgak release`, the tray's *Release stuck keys*), and sends an up, marked as
//!   Ddalgak's own, for each stale one. Nothing polls (`DDALGAK-NFR-001`).

use super::inject::SendInputBackend;
use super::wide;
use crate::inject::{Backend, OutCode};
use crate::recovery::{self, Scope};
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

/// `LowLevelHooksTimeout` of the user, in ms (`None`: absent or unreadable).
pub fn read_hook_timeout() -> Option<u32> {
    let key = wide(r"Control Panel\Desktop");
    let name = wide("LowLevelHooksTimeout");
    let mut buf = [0u16; 32];
    let mut bytes = u32::try_from(size_of_val(&buf)).expect("small");
    let mut kind = 0u32;
    // SAFETY: HKCU is a predefined key; `buf` has `bytes` writable bytes; the value is a string
    // or a DWORD (both fit), read only.
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ | RRF_RT_REG_DWORD,
            &raw mut kind,
            buf.as_mut_ptr().cast(),
            &raw mut bytes,
        )
    };
    if rc != ERROR_SUCCESS {
        return None;
    }
    // REG_DWORD = 4.
    if kind == 4 {
        return Some(u32::from(buf[0]) | (u32::from(buf[1]) << 16));
    }
    let len = (bytes as usize / 2).min(buf.len());
    recovery::parse_timeout(&String::from_utf16_lossy(&buf[..len]))
}

/// The threshold the hook uses (µs): the registry value capped at 1 s, or the assumption when it
/// is absent. Debug builds of the test host may lower it (`DDALGAK_DEV_HOOK_TIMEOUT_US`) so a
/// test can drive the whole path with one ordinary callback.
pub fn hook_timeout_us(test_host: bool) -> u64 {
    if cfg!(debug_assertions)
        && test_host
        && let Some(us) = std::env::var("DDALGAK_DEV_HOOK_TIMEOUT_US")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
    {
        return us;
    }
    u64::from(recovery::hook_timeout_ms(read_hook_timeout())) * 1000
}

/// Whether Windows considers the virtual key `vk` pressed right now.
pub fn key_down(vk: u16) -> bool {
    // SAFETY: no preconditions; reads the asynchronous key state of this desktop.
    (unsafe { GetAsyncKeyState(i32::from(vk)) } as u16) & 0x8000 != 0
}

/// What a release did.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Released {
    /// Candidate keys and buttons checked.
    pub checked: usize,
    /// Stale ones an up was sent for (or would be, in a dry run).
    pub released: usize,
}

/// Releases the `candidates` Windows reports pressed that nobody holds (see
/// [`recovery::stale`]). `dry_run` only counts.
pub fn release_stale(
    candidates: &[OutCode],
    physical: Option<u8>,
    ledger: &[OutCode],
    scope: Scope,
    dry_run: bool,
) -> Released {
    let checked: Vec<OutCode> = candidates
        .iter()
        .copied()
        .filter(|c| recovery::in_scope(*c, scope))
        .collect();
    let stale = recovery::stale(&checked, &key_down, physical, ledger, scope);
    if !dry_run && !stale.is_empty() {
        let events = recovery::release_events(&stale);
        // Marked as Ddalgak's own: the hook passes them on without matching any rule.
        let _ = SendInputBackend.send(&events);
    }
    Released {
        checked: checked.len(),
        released: stale.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hook_timeout_is_read_without_changing_anything() {
        // Read-only; on this machine the value is usually absent (then the assumption applies).
        let v = read_hook_timeout();
        let us = hook_timeout_us(false);
        assert!(us > 0 && us <= 1_000_000, "{v:?} -> {us} µs");
        if v.is_none() {
            assert_eq!(us, u64::from(recovery::ASSUMED_HOOK_TIMEOUT_MS) * 1000);
        }
    }

    #[test]
    fn a_release_with_nothing_stale_sends_nothing() {
        // F24 is never held on this machine: nothing to release, nothing sent.
        let r = release_stale(&[OutCode::Vk(0x87)], None, &[], Scope::ObscureOnly, false);
        assert_eq!(r.checked, 1);
        if !key_down(0x87) {
            assert_eq!(r.released, 0);
        }
        // Out of scope for the test host: modifiers are not even checked.
        let r = release_stale(&[OutCode::Vk(0xA2)], None, &[], Scope::ObscureOnly, true);
        assert_eq!(r, Released::default());
    }
}
