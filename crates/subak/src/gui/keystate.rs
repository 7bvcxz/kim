//! Where the Subak window and the floating UI take the modifier state of a key or mouse message
//! from (Step 21 fix of the Step 16–20 off-screen tests).
//!
//! A real window reads this thread's synchronous key state (`GetKeyState`), as before: it is the
//! state at the time of the message being processed. The off-screen test window never has the
//! keyboard, so whatever key state its thread sees can only leak in from the machine's real
//! keyboard: a modifier the user holds in another application while the tests run, or the input
//! another test synthesizes at that moment (Step 20's merge gate saw two Menu-key tests fail at the
//! same moment as a Ddalgak real-input test, the Menu key having become `Ctrl+Menu`). Under the
//! test mode ([`ignore_real_keyboard`]) every key reads as up, and the tests carry modifiers in what
//! they send: the `key` test command, `MK_SHIFT`/`MK_CONTROL` in a mouse message's `wParam`, and
//! the context bit (29, Alt held) of a `WM_SYSKEYDOWN`/`WM_SYSCHAR` they post ([`with_message`]).

use super::keys::vk;
use std::cell::Cell;
use subak_term::Mods;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetKeyState;
use windows_sys::Win32::UI::WindowsAndMessaging::{WM_SYSCHAR, WM_SYSKEYDOWN};

thread_local! {
    static IGNORE: Cell<bool> = const { Cell::new(false) };
}

/// Turns the real keyboard's state off (`true`, the off-screen test mode) or on for this thread.
pub fn ignore_real_keyboard(ignore: bool) {
    IGNORE.set(ignore);
}

/// Whether virtual key `k` is down for the message being processed (always up under the test
/// mode, see the module documentation).
pub fn down(k: u32) -> bool {
    // SAFETY: a plain query of this thread's key-state table.
    !IGNORE.get() && unsafe { GetKeyState(k as i32) } < 0
}

/// Shift, Ctrl and Alt as held for the message being processed.
pub fn held() -> Mods {
    Mods {
        shift: down(vk::SHIFT),
        ctrl: down(vk::CONTROL),
        alt: down(vk::MENU),
    }
}

/// [`held`], plus what the keyboard message itself records: a `WM_SYSKEYDOWN` or `WM_SYSCHAR`
/// with its context bit (29) set was made with Alt held. A real window's key state already
/// says so; under the test mode this is the only way a posted message carries Alt.
pub fn with_message(msg: u32, lp: isize) -> Mods {
    let mut m = held();
    if IGNORE.get() && matches!(msg, WM_SYSKEYDOWN | WM_SYSCHAR) && (lp >> 29) & 1 == 1 {
        m.alt = true;
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetKeyboardState, SetKeyboardState};
    use windows_sys::Win32::UI::WindowsAndMessaging::WM_KEYDOWN;

    /// The key state is this test thread's own table (`SetKeyboardState` generates no input and
    /// changes no other thread): with modifiers "held" in it, a real window reads them and the
    /// test mode does not, and a posted system key carries Alt in its context bit.
    #[test]
    fn the_test_mode_reads_no_real_key_state_and_messages_carry_their_modifiers() {
        let mut keys = [0u8; 256];
        // SAFETY: this thread's key-state table, read, changed and restored below.
        let saved = unsafe {
            GetKeyboardState(keys.as_mut_ptr());
            let saved = keys;
            for k in [vk::SHIFT, vk::CONTROL, vk::MENU, vk::LCONTROL] {
                keys[k as usize] = 0x80;
            }
            SetKeyboardState(keys.as_ptr());
            saved
        };
        ignore_real_keyboard(false);
        let real = held();
        assert!(
            real.shift && real.ctrl && real.alt,
            "a real window reads them"
        );
        assert!(down(vk::LCONTROL));
        ignore_real_keyboard(true);
        assert_eq!(held(), Mods::NONE, "the test window ignores held keys");
        assert!(!down(vk::LCONTROL));
        assert_eq!(with_message(WM_KEYDOWN, 1 << 29), Mods::NONE);
        assert_eq!(
            with_message(WM_SYSKEYDOWN, 1 | (1 << 29)),
            Mods {
                alt: true,
                ..Mods::NONE
            },
            "Alt from the message's context bit"
        );
        assert_eq!(
            with_message(WM_SYSKEYDOWN, 1),
            Mods::NONE,
            "F10 without Alt"
        );
        ignore_real_keyboard(false);
        // SAFETY: restores this thread's key-state table.
        unsafe { SetKeyboardState(saved.as_ptr()) };
    }
}
