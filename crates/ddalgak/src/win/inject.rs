//! `SendInput` with Ddalgak's marker (`DDALGAK-SEC-004`).
//!
//! Every event carries [`OWN_MARKER`] in `dwExtraInfo`; Windows adds `LLKHF_INJECTED`. The hook
//! sees both and passes the event on without matching it. Keys are sent by virtual key with the
//! layout's scan code (`MapVirtualKeyW`), so applications that read scan codes see the right
//! key. UIPI makes `SendInput` into a higher-integrity window fail silently (*documented*);
//! injecting rules are therefore skipped over elevated windows before they run
//! (`skipped_elevated_target`).

use crate::event::OWN_MARKER;
use crate::inject::{Backend, OutKey};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, MAPVK_VK_TO_VSC, MapVirtualKeyW,
    SendInput,
};

fn inputs(keys: &[OutKey], extra: usize) -> Vec<INPUT> {
    keys.iter()
        .map(|k| {
            // SAFETY: a pure table lookup for the current layout; no pointers.
            let scan = unsafe { MapVirtualKeyW(u32::from(k.vk), MAPVK_VK_TO_VSC) };
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: k.vk,
                        wScan: u16::try_from(scan).unwrap_or(0),
                        dwFlags: if k.up { KEYEVENTF_KEYUP } else { 0 },
                        time: 0,
                        dwExtraInfo: extra,
                    },
                },
            }
        })
        .collect()
}

fn send(keys: &[OutKey], extra: usize) -> usize {
    if keys.is_empty() {
        return 0;
    }
    let inputs = inputs(keys, extra);
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
    fn send(&mut self, keys: &[OutKey]) -> usize {
        send(keys, OWN_MARKER)
    }
}

/// Injects keys **without** Ddalgak's marker, as another program would. Tests use it (with the
/// obscure keys F13–F20 only) to exercise the `injected: ignore|accept` policy; the engine never
/// calls it.
pub fn send_as_other_program(keys: &[OutKey]) -> usize {
    send(keys, 0)
}
