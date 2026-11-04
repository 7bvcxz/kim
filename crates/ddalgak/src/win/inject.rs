//! `SendInput` with Ddalgak's marker (`DDALGAK-SEC-004`).
//!
//! Every event carries [`OWN_MARKER`] in `dwExtraInfo`; Windows adds `LLKHF_INJECTED` (keys) or
//! `LLMHF_INJECTED` (mouse). The hooks see both and pass the event on without matching it.
//!
//! - **Virtual keys** are sent with the layout's scan code (`MapVirtualKeyW(…, MAPVK_VK_TO_VSC_EX)`)
//!   and `KEYEVENTF_EXTENDEDKEY` when that scan code has the `E0`/`E1` prefix, so applications that
//!   read scan codes see the right key (Up is not Numpad8, Right Ctrl is not Left Ctrl).
//! - **Scan codes** (`code:` outputs) are sent with `KEYEVENTF_SCANCODE`: Windows turns the
//!   position into the active layout's key.
//! - **Mouse buttons** are `MOUSEINPUT` button events. `MouseLeft`/`MouseRight` are the logical
//!   primary and secondary buttons: with "swap buttons" on, the physical events are swapped
//!   (DDALGAK_RULE_MODEL §5.9).
//!
//! UIPI makes `SendInput` into a higher-integrity window fail silently (*documented*); injecting
//! rules are therefore skipped over elevated windows before they run
//! (`skipped_elevated_target`).

use crate::event::OWN_MARKER;
use crate::inject::{Backend, Button, OutCode, OutEvent, OutKey};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MAPVK_VK_TO_VSC_EX, MOUSE_EVENT_FLAGS,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT,
    MapVirtualKeyW, SendInput,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_SWAPBUTTON, XBUTTON1, XBUTTON2,
};

/// The physical button event for a logical button (`swapped`: the user swapped the buttons).
/// Returns the `MOUSEINPUT` flags and `mouseData`.
pub fn button_flags(b: Button, up: bool, swapped: bool) -> (MOUSE_EVENT_FLAGS, u32) {
    let b = match (b, swapped) {
        (Button::Left, true) => Button::Right,
        (Button::Right, true) => Button::Left,
        (b, _) => b,
    };
    match (b, up) {
        (Button::Left, false) => (MOUSEEVENTF_LEFTDOWN, 0),
        (Button::Left, true) => (MOUSEEVENTF_LEFTUP, 0),
        (Button::Right, false) => (MOUSEEVENTF_RIGHTDOWN, 0),
        (Button::Right, true) => (MOUSEEVENTF_RIGHTUP, 0),
        (Button::Middle, false) => (MOUSEEVENTF_MIDDLEDOWN, 0),
        (Button::Middle, true) => (MOUSEEVENTF_MIDDLEUP, 0),
        (Button::X1, false) => (MOUSEEVENTF_XDOWN, u32::from(XBUTTON1)),
        (Button::X1, true) => (MOUSEEVENTF_XUP, u32::from(XBUTTON1)),
        (Button::X2, false) => (MOUSEEVENTF_XDOWN, u32::from(XBUTTON2)),
        (Button::X2, true) => (MOUSEEVENTF_XUP, u32::from(XBUTTON2)),
    }
}

fn key_input(vk: u16, scan: u16, flags: u32, extra: usize) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: extra,
            },
        },
    }
}

fn inputs(events: &[OutEvent], extra: usize) -> Vec<INPUT> {
    // SAFETY: no preconditions; reads one system metric.
    let swapped = events.iter().any(|e| matches!(e.code, OutCode::Button(_)))
        && unsafe { GetSystemMetrics(SM_SWAPBUTTON) } != 0;
    events
        .iter()
        .map(|e| {
            let up = if e.up { KEYEVENTF_KEYUP } else { 0 };
            match e.code {
                OutCode::Vk(vk) | OutCode::VkExtended(vk) => {
                    // SAFETY: a pure table lookup for the current layout; no pointers.
                    let sc = unsafe { MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC_EX) };
                    let ext = matches!(e.code, OutCode::VkExtended(_))
                        || matches!(sc >> 8, 0xE0 | 0xE1)
                        || crate::vk::is_extended_vk(vk);
                    let flags = up | if ext { KEYEVENTF_EXTENDEDKEY } else { 0 };
                    key_input(vk, u16::try_from(sc & 0xFF).unwrap_or(0), flags, extra)
                }
                OutCode::Scan(scan) => key_input(0, scan, up | KEYEVENTF_SCANCODE, extra),
                OutCode::Exact { vk, scan, ext } => {
                    let flags = up | if ext { KEYEVENTF_EXTENDEDKEY } else { 0 };
                    key_input(vk, scan, flags, extra)
                }
                OutCode::Button(b) => {
                    let (flags, data) = button_flags(b, e.up, swapped);
                    INPUT {
                        r#type: INPUT_MOUSE,
                        Anonymous: INPUT_0 {
                            mi: MOUSEINPUT {
                                dx: 0,
                                dy: 0,
                                mouseData: data,
                                dwFlags: flags,
                                time: 0,
                                dwExtraInfo: extra,
                            },
                        },
                    }
                }
            }
        })
        .collect()
}

fn send(events: &[OutEvent], extra: usize) -> usize {
    if events.is_empty() {
        return 0;
    }
    let inputs = inputs(events, extra);
    // SAFETY: `inputs` holds `len` initialized INPUT structures of the size passed.
    let n = unsafe {
        SendInput(
            u32::try_from(inputs.len()).unwrap_or(0),
            inputs.as_ptr(),
            i32::try_from(size_of::<INPUT>()).expect("small"),
        )
    };
    n as usize
}

/// The real backend: marked `SendInput`.
#[derive(Clone, Copy, Debug, Default)]
pub struct SendInputBackend;

impl Backend for SendInputBackend {
    fn send(&mut self, events: &[OutEvent]) -> usize {
        send(events, OWN_MARKER)
    }
}

/// Injects keys **without** Ddalgak's marker, as another program would. Tests use it (with the
/// obscure keys F13–F20 only) to exercise the `injected: ignore|accept` policy; the engine never
/// calls it.
pub fn send_as_other_program(keys: &[OutKey]) -> usize {
    let events: Vec<OutEvent> = keys.iter().map(|k| OutEvent::from(*k)).collect();
    send(&events, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_buttons_follow_the_swap_setting() {
        assert_eq!(
            button_flags(Button::Left, false, false).0,
            MOUSEEVENTF_LEFTDOWN
        );
        assert_eq!(
            button_flags(Button::Left, false, true).0,
            MOUSEEVENTF_RIGHTDOWN,
            "primary = the right button when swapped"
        );
        assert_eq!(
            button_flags(Button::Right, true, true).0,
            MOUSEEVENTF_LEFTUP
        );
        assert_eq!(
            button_flags(Button::Middle, false, true).0,
            MOUSEEVENTF_MIDDLEDOWN
        );
        assert_eq!(
            button_flags(Button::X2, true, false),
            (MOUSEEVENTF_XUP, u32::from(XBUTTON2))
        );
    }

    #[test]
    fn extended_keys_get_the_extended_flag_and_codes_their_scan_code() {
        // Pure table lookups on this machine's layout; nothing is sent.
        let up_arrow = inputs(&[OutEvent::down(OutCode::Vk(0x26))], 0);
        // SAFETY: reading the union field the constructor wrote.
        let ki = unsafe { up_arrow[0].Anonymous.ki };
        assert_eq!(ki.wVk, 0x26);
        assert_ne!(
            ki.dwFlags & KEYEVENTF_EXTENDEDKEY,
            0,
            "Up is an extended key"
        );
        let enter = inputs(&[OutEvent::up(OutCode::VkExtended(0x0D))], 0);
        // SAFETY: as above.
        let ki = unsafe { enter[0].Anonymous.ki };
        assert_ne!(ki.dwFlags & KEYEVENTF_EXTENDEDKEY, 0, "NumpadEnter");
        assert_ne!(ki.dwFlags & KEYEVENTF_KEYUP, 0);
        let f16 = inputs(&[OutEvent::down(OutCode::Vk(0x7F))], 0);
        // SAFETY: as above.
        let ki = unsafe { f16[0].Anonymous.ki };
        assert_eq!(ki.dwFlags & KEYEVENTF_EXTENDEDKEY, 0, "F16 is not extended");
        let h = inputs(&[OutEvent::down(OutCode::Scan(0x23))], OWN_MARKER);
        // SAFETY: as above.
        let ki = unsafe { h[0].Anonymous.ki };
        assert_eq!((ki.wVk, ki.wScan), (0, 0x23));
        assert_ne!(ki.dwFlags & KEYEVENTF_SCANCODE, 0);
        assert_eq!(ki.dwExtraInfo, OWN_MARKER);
    }
}
