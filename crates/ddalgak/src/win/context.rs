//! Context watchers for stepping aside (DDALGAK_SAFETY §3, §5, §8; ADR 0024 §2).
//!
//! Out-of-context WinEvent hooks (`WINEVENT_OUTOFCONTEXT`: no DLL is loaded into any process,
//! callbacks arrive through the installing thread's message loop) keep the hook's flags current,
//! so the hook callback reads atomics instead of asking other windows:
//!
//! | Event | Flag |
//! | --- | --- |
//! | `EVENT_SYSTEM_FOREGROUND` | the foreground process has a higher integrity level (`skipped_elevated_target` for injecting rules; when it becomes elevated, held remap outputs are released and the key state is forgotten, because its keys never reach the hook); focus epoch (cancels sequences); IME composition reset |
//! | `EVENT_OBJECT_FOCUS` | the focused control is a Win32 password edit (`ES_PASSWORD`; `skipped_password_field`) |
//! | `EVENT_OBJECT_IME_SHOW` / `_HIDE` | an IME composition or candidate window is shown (`skipped_ime_composition`) |
//! | `EVENT_SYSTEM_DESKTOPSWITCH` | input went to another desktop (lock screen, UAC, Ctrl+Alt+Del): the hook forgets its key state, because the key-ups made there never reach it |
//!
//! Limits, stated honestly: password fields drawn by browsers, UWP apps or custom controls are
//! not recognized here (UI Automation `IsPassword` follows in Step 69, best effort per
//! DDALGAK_SAFETY §8); IMEs that compose inline without showing an IME window may not raise the
//! IME events (verified with the Korean IME in Steps 68–69). The watchers read window classes,
//! styles and process integrity only: never titles, text or keys (`DDALGAK-SEC-011`). They are
//! installed only while the engine is on.

use super::hook::Shared;
use super::{own_integrity, process_integrity};
use std::cell::RefCell;
use std::sync::Arc;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    ES_PASSWORD, EVENT_OBJECT_FOCUS, EVENT_OBJECT_IME_HIDE, EVENT_OBJECT_IME_SHOW,
    EVENT_SYSTEM_DESKTOPSWITCH, EVENT_SYSTEM_FOREGROUND, GUITHREADINFO, GWL_STYLE, GetClassNameW,
    GetForegroundWindow, GetGUIThreadInfo, GetWindowLongW, GetWindowThreadProcessId,
    WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
};

struct Ctx {
    shared: Arc<Shared>,
    own_il: u32,
}

thread_local! {
    static CTX: RefCell<Option<Ctx>> = const { RefCell::new(None) };
}

/// Whether `hwnd` is a Win32 edit control with the `ES_PASSWORD` style.
pub(crate) fn is_password_window(hwnd: HWND) -> bool {
    if hwnd.is_null() {
        return false;
    }
    let mut class = [0u16; 64];
    // SAFETY: `class` has room for the given number of UTF-16 units.
    let n = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), 64) };
    let name = String::from_utf16_lossy(&class[..usize::try_from(n).unwrap_or(0)]);
    let editlike =
        name.eq_ignore_ascii_case("Edit") || name.to_ascii_uppercase().starts_with("RICHEDIT");
    // SAFETY: reads a window's style; works for windows of other processes (documented).
    let style = unsafe { GetWindowLongW(hwnd, GWL_STYLE) };
    editlike && (style as u32) & (ES_PASSWORD as u32) != 0
}

/// Whether the process owning `hwnd` runs at a higher integrity level than Ddalgak. A process
/// whose level cannot be read counts as higher (stepping aside is the safe side).
pub(crate) fn is_elevated_window(hwnd: HWND, own_il: u32) -> bool {
    if hwnd.is_null() {
        return false;
    }
    let mut pid = 0u32;
    // SAFETY: `pid` is writable; an invalid window yields 0.
    unsafe { GetWindowThreadProcessId(hwnd, &raw mut pid) };
    if pid == 0 || pid == std::process::id() {
        return false;
    }
    process_integrity(pid).map_or(true, |il| il > own_il)
}

/// The focused window of the foreground thread.
fn focused_window() -> HWND {
    let mut info = GUITHREADINFO {
        cbSize: u32::try_from(size_of::<GUITHREADINFO>()).expect("small"),
        ..Default::default()
    };
    // SAFETY: thread id 0 = the foreground thread; `info` is a sized out-parameter.
    if unsafe { GetGUIThreadInfo(0, &raw mut info) } == 0 {
        return std::ptr::null_mut();
    }
    info.hwndFocus
}

fn refresh(ctx: &Ctx, foreground: HWND) {
    let elevated = is_elevated_window(foreground, ctx.own_il);
    let was = ctx.shared.flags().elevated_foreground;
    ctx.shared.set_elevated_foreground(elevated);
    ctx.shared
        .set_password_field(is_password_window(focused_window()));
    if elevated && !was {
        // Keys typed into an elevated window never reach this hook (UIPI), so a held remap's
        // trigger-up would be lost: release every output and forget the key state now
        // (`DDALGAK-SEC-005`).
        ctx.shared.request_key_reset();
    }
}

unsafe extern "system" fn on_event(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    _object: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    CTX.with(|c| {
        let Ok(guard) = c.try_borrow() else {
            return;
        };
        let Some(ctx) = guard.as_ref() else {
            return;
        };
        match event {
            EVENT_SYSTEM_FOREGROUND => {
                ctx.shared.bump_focus();
                ctx.shared.set_ime_composing(false);
                refresh(ctx, hwnd);
            }
            EVENT_OBJECT_FOCUS => {
                ctx.shared.bump_focus();
                ctx.shared.set_password_field(is_password_window(hwnd));
            }
            EVENT_SYSTEM_DESKTOPSWITCH => ctx.shared.request_key_reset(),
            EVENT_OBJECT_IME_SHOW => ctx.shared.set_ime_composing(true),
            EVENT_OBJECT_IME_HIDE => ctx.shared.set_ime_composing(false),
            _ => {}
        }
    });
}

/// The installed watchers; removed on drop. Must live on a thread with a message loop.
#[derive(Debug)]
pub struct Watcher {
    hooks: Vec<isize>,
}

impl Watcher {
    pub fn install(shared: Arc<Shared>) -> Watcher {
        let own_il = own_integrity().unwrap_or(0x2000);
        let ctx = Ctx { shared, own_il };
        // SAFETY: no preconditions.
        refresh(&ctx, unsafe { GetForegroundWindow() });
        CTX.with(|c| *c.borrow_mut() = Some(ctx));
        let mut hooks = Vec::new();
        for (min, max) in [
            (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
            (EVENT_SYSTEM_DESKTOPSWITCH, EVENT_SYSTEM_DESKTOPSWITCH),
            (EVENT_OBJECT_FOCUS, EVENT_OBJECT_FOCUS),
            (EVENT_OBJECT_IME_SHOW, EVENT_OBJECT_IME_HIDE),
        ] {
            // SAFETY: out-of-context hook (no module, no DLL); the callback has the documented
            // signature and runs on this thread's message loop.
            let h = unsafe {
                SetWinEventHook(
                    min,
                    max,
                    std::ptr::null_mut(),
                    Some(on_event),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
                )
            };
            if !h.is_null() {
                hooks.push(h as isize);
            }
        }
        Watcher { hooks }
    }

    pub fn count(&self) -> usize {
        self.hooks.len()
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        for h in self.hooks.drain(..) {
            // SAFETY: a WinEvent hook installed by this thread, removed once.
            unsafe { UnhookWinEvent(h as HWINEVENTHOOK) };
        }
        CTX.with(|c| c.borrow_mut().take());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::wide;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, ES_PASSWORD, HWND_MESSAGE, WS_CHILD,
    };

    fn edit(style: u32) -> HWND {
        let class = wide("EDIT");
        // SAFETY: creates a hidden child of the message-only parent (never shown).
        let parent = unsafe {
            CreateWindowExW(
                0,
                wide("STATIC").as_ptr(),
                std::ptr::null(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        assert!(!parent.is_null());
        // SAFETY: as above; WS_CHILD without WS_VISIBLE: never shown.
        let h = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                std::ptr::null(),
                WS_CHILD | style,
                0,
                0,
                10,
                10,
                parent,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        assert!(!h.is_null());
        h
    }

    #[test]
    fn password_edits_are_recognized_without_showing_anything() {
        let pw = edit(ES_PASSWORD as u32);
        let plain = edit(0);
        assert!(is_password_window(pw));
        assert!(!is_password_window(plain));
        assert!(!is_password_window(std::ptr::null_mut()));
        // Our own windows are never "elevated".
        assert!(!is_elevated_window(pw, own_integrity().unwrap()));
        // SAFETY: windows created by this thread.
        unsafe {
            DestroyWindow(pw);
            DestroyWindow(plain);
        }
    }
}
