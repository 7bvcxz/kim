//! Win32 pin windows: borderless, always-on-top, layered tool windows (NOEUL-FR-060, ADR 0023).
//!
//! - **Never on the taskbar or in Alt+Tab** (`WS_EX_TOOLWINDOW`, no `WS_EX_APPWINDOW`).
//! - **100 %**: the picture is uploaded once with `UpdateLayeredWindow` from a premultiplied DIB
//!   section of exactly its size. No `WM_PAINT`, no timer, no GPU context: an idle pin costs no
//!   CPU (NOEUL-NFR-001), and about its pixel bytes twice (the frame and the DIB, NOEUL-NFR-002).
//! - **Focus only when asked**: shown with `SW_SHOWNOACTIVATE` unless the user created it (then it
//!   is brought to the foreground so `Esc` and the arrows work at once). Clicking it activates it.
//! - **Input stays Noeul's own**: keys reach the pin only while it has focus; they are matched
//!   against the `pin` context and never forwarded anywhere (NOEUL-SEC-012). The IME is switched
//!   off for the window so letter shortcuts work in Hangul mode (NOEUL-NFR-014).
//! - Dragging anywhere moves it (the system move loop, `HTCAPTION`). A double-click does nothing
//!   (NOEUL-FR-026). Mouse zoom, resizing and the menu are Step 53.
//!
//! The window procedure only queues [`PinEvent`]s and calls the registered waker; the instance
//! core handles them later, never re-entrantly.

use crate::app::PinEvent;
use crate::frame::Frame;
use crate::hotkeys::chord_of;
use crate::pins::{PinId, PinWindow, PinWindows};
use crate::win::wide;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::OnceLock;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, HBITMAP,
    HDC, ReleaseDC, SelectObject,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::Ime::ImmAssociateContextEx;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_MENU, VK_SHIFT};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CS_DBLCLKS, CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA, GetWindowLongPtrW,
    GetWindowRect, HTCAPTION, IDC_SIZEALL, IsWindow, LoadCursorW, RegisterClassExW, SW_SHOW,
    SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetForegroundWindow,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, ULW_ALPHA, UpdateLayeredWindow, WM_CLOSE,
    WM_DESTROY, WM_EXITSIZEMOVE, WM_KEYDOWN, WM_NCHITTEST, WM_NCLBUTTONDBLCLK, WM_NCRBUTTONDBLCLK,
    WM_SYSKEYDOWN, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

const CLASS: &str = "NoeulPin";

thread_local! {
    static EVENTS: RefCell<VecDeque<PinEvent>> = const { RefCell::new(VecDeque::new()) };
    static WAKER: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
}

/// Sets what runs after a pin window queued an event (the instance's pump). Same thread.
pub fn set_waker(f: Option<Box<dyn Fn()>>) {
    WAKER.with(|w| *w.borrow_mut() = f);
}

/// Takes the queued pin events (in order).
pub fn take_events() -> Vec<PinEvent> {
    EVENTS.with(|e| e.borrow_mut().drain(..).collect())
}

fn push(e: PinEvent) {
    EVENTS.with(|q| q.borrow_mut().push_back(e));
    // A waker that is already running (re-entrant dispatch) is not called again: the outer
    // pump drains the queue when it finishes.
    WAKER.with(|w| {
        if let Ok(w) = w.try_borrow()
            && let Some(f) = w.as_ref()
        {
            f();
        }
    });
}

/// The logical key name of a virtual key, as the hotkey contract spells it (§5.2).
pub fn vk_name(vk: u32) -> Option<String> {
    Some(match vk {
        0x41..=0x5A | 0x30..=0x39 => char::from(u8::try_from(vk).ok()?).to_string(),
        0x70..=0x87 => format!("F{}", vk - 0x6F),
        0x60..=0x69 => format!("Numpad{}", vk - 0x60),
        0x1B => "Escape".into(),
        0x09 => "Tab".into(),
        0x0D => "Enter".into(),
        0x20 => "Space".into(),
        0x08 => "Backspace".into(),
        0x2D => "Insert".into(),
        0x2E => "Delete".into(),
        0x24 => "Home".into(),
        0x23 => "End".into(),
        0x21 => "PageUp".into(),
        0x22 => "PageDown".into(),
        0x26 => "Up".into(),
        0x28 => "Down".into(),
        0x25 => "Left".into(),
        0x27 => "Right".into(),
        0xBB => "Plus".into(),
        0xBD => "Minus".into(),
        0xBC => "Comma".into(),
        0xBE => "Period".into(),
        0x6A => "NumpadMultiply".into(),
        0x6B => "NumpadAdd".into(),
        0x6D => "NumpadSubtract".into(),
        0x6E => "NumpadDecimal".into(),
        0x6F => "NumpadDivide".into(),
        _ => return None,
    })
}

fn down(vk: u16) -> bool {
    // SAFETY: plain key-state query for this thread's input.
    unsafe { GetKeyState(i32::from(vk)) < 0 }
}

fn pin_of(hwnd: HWND) -> Option<PinId> {
    // SAFETY: reads the value stored by `create` (0 before it is set).
    let v = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
    u64::try_from(v).ok().filter(|v| *v > 0).map(PinId)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        // Drag anywhere to move.
        WM_NCHITTEST => return HTCAPTION as LRESULT,
        // A double-click on a pin has no default action (NOEUL-FR-026); never maximize.
        WM_NCLBUTTONDBLCLK | WM_NCRBUTTONDBLCLK => return 0,
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let vk = u32::try_from(wp).unwrap_or(0);
            let repeat = (lp >> 30) & 1 == 1;
            if let (Some(id), Some(name)) = (pin_of(hwnd), vk_name(vk))
                && let Some(chord) =
                    chord_of(&name, down(VK_CONTROL), down(VK_MENU), down(VK_SHIFT))
            {
                // Alt+F4 stays the system's (reserved `window.close`): it arrives as WM_CLOSE.
                if !(msg == WM_SYSKEYDOWN && name == "F4") {
                    push(PinEvent::Key(id, chord, repeat));
                    return 0;
                }
            }
        }
        WM_EXITSIZEMOVE => {
            let mut r = RECT::default();
            // SAFETY: `hwnd` is this window; `r` is writable.
            let ok = unsafe { GetWindowRect(hwnd, &raw mut r) } != 0;
            if let Some(id) = pin_of(hwnd)
                && ok
            {
                push(PinEvent::Moved(id, r.left, r.top));
            }
            return 0;
        }
        WM_CLOSE => {
            // Alt+F4 or the system: the core closes the pin (and destroys this window).
            if let Some(id) = pin_of(hwnd) {
                push(PinEvent::Closed(id));
            }
            return 0;
        }
        WM_DESTROY => return 0,
        _ => {}
    }
    // SAFETY: default processing of a message of our own window.
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

fn class() -> Result<*const u16, String> {
    static ATOM: OnceLock<Result<usize, String>> = OnceLock::new();
    let name = ATOM.get_or_init(|| {
        let name = wide(CLASS);
        let wc = WNDCLASSEXW {
            cbSize: u32::try_from(size_of::<WNDCLASSEXW>()).expect("small"),
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            // SAFETY: the module handle of this executable.
            hInstance: unsafe { GetModuleHandleW(std::ptr::null()) },
            // SAFETY: a system cursor.
            hCursor: unsafe { LoadCursorW(std::ptr::null_mut(), IDC_SIZEALL) },
            lpszClassName: name.as_ptr(),
            ..Default::default()
        };
        // SAFETY: `wc` is fully initialized; the class name is copied by the system.
        if unsafe { RegisterClassExW(&raw const wc) } == 0 {
            return Err(format!(
                "RegisterClassExW failed ({})",
                std::io::Error::last_os_error()
            ));
        }
        // Keep the name alive for CreateWindowExW (leaked once per process).
        Ok(Box::leak(name.into_boxed_slice()).as_ptr() as usize)
    });
    name.clone().map(|p| p as *const u16)
}

/// A premultiplied BGRA DIB section with the picture.
struct Surface {
    dc: HDC,
    bitmap: HBITMAP,
    old: *mut c_void,
}

impl Surface {
    fn new(frame: &Frame) -> Result<Surface, String> {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: u32::try_from(size_of::<BITMAPINFOHEADER>()).expect("small"),
                biWidth: i32::try_from(frame.width()).map_err(|_| "too wide")?,
                biHeight: -i32::try_from(frame.height()).map_err(|_| "too high")?,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..Default::default()
            },
            ..Default::default()
        };
        // SAFETY: a memory DC compatible with the screen; deleted in Drop.
        let dc = unsafe { CreateCompatibleDC(std::ptr::null_mut()) };
        if dc.is_null() {
            return Err("CreateCompatibleDC failed".into());
        }
        let mut bits: *mut c_void = std::ptr::null_mut();
        // SAFETY: `info` describes a top-down 32-bit DIB of the frame's size.
        let bitmap = unsafe {
            CreateDIBSection(
                dc,
                &raw const info,
                DIB_RGB_COLORS,
                &raw mut bits,
                std::ptr::null_mut(),
                0,
            )
        };
        if bitmap.is_null() || bits.is_null() {
            // SAFETY: the DC created above.
            unsafe { DeleteDC(dc) };
            return Err("CreateDIBSection failed (out of memory?)".into());
        }
        // SAFETY: the DIB holds width×height 32-bit pixels at `bits`.
        let dst =
            unsafe { std::slice::from_raw_parts_mut(bits.cast::<u8>(), frame.pixels().len()) };
        // UpdateLayeredWindow wants premultiplied alpha.
        for (d, s) in dst
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(frame.pixels().as_chunks::<4>().0)
        {
            let a = u16::from(s[3]);
            let pm = |c: u8| u8::try_from((u16::from(c) * a + 127) / 255).unwrap_or(255);
            *d = [pm(s[0]), pm(s[1]), pm(s[2]), s[3]];
        }
        // SAFETY: selects our bitmap; restored in Drop.
        let old = unsafe { SelectObject(dc, bitmap) };
        Ok(Surface { dc, bitmap, old })
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        // SAFETY: objects created in `new`, released once in reverse order.
        unsafe {
            SelectObject(self.dc, self.old);
            DeleteObject(self.bitmap);
            DeleteDC(self.dc);
        }
    }
}

/// One live pin window.
pub struct WinPin {
    hwnd: HWND,
    _surface: Surface,
}

impl std::fmt::Debug for WinPin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WinPin")
            .field("hwnd", &self.hwnd)
            .finish_non_exhaustive()
    }
}

impl WinPin {
    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }
}

impl PinWindow for WinPin {
    fn set_position(&mut self, x: i32, y: i32) {
        // SAFETY: our window; moves it without activating or resizing.
        unsafe {
            SetWindowPos(
                self.hwnd,
                std::ptr::null_mut(),
                x,
                y,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            )
        };
    }

    fn focus(&mut self) -> bool {
        // SAFETY: our window; the OS may refuse (foreground lock), which is reported.
        unsafe { SetForegroundWindow(self.hwnd) != 0 }
    }

    fn destroy(&mut self) {
        // SAFETY: destroys our own window once; later calls see a dead handle and do nothing.
        unsafe {
            if !self.hwnd.is_null() && IsWindow(self.hwnd) != 0 {
                SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
                DestroyWindow(self.hwnd);
            }
        }
        self.hwnd = std::ptr::null_mut();
    }
}

impl Drop for WinPin {
    fn drop(&mut self) {
        self.destroy();
    }
}

/// Creates Win32 pin windows on the calling (UI) thread.
#[derive(Debug, Default)]
pub struct WinPins;

/// Creates one pin window. Exposed for tests, which place it off-screen.
pub fn create(id: PinId, frame: &Frame, focus: bool) -> Result<WinPin, String> {
    let class = class()?;
    let r = frame.rect();
    let (w, h) = (
        i32::try_from(r.width).map_err(|_| "too wide")?,
        i32::try_from(r.height).map_err(|_| "too high")?,
    );
    let title = wide(format!("Noeul {id}"));
    // SAFETY: registered class; plain values; no parent, menu or creation data.
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            class,
            title.as_ptr(),
            WS_POPUP,
            r.x,
            r.y,
            w,
            h,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            GetModuleHandleW(std::ptr::null()),
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        return Err(format!(
            "CreateWindowExW failed ({})",
            std::io::Error::last_os_error()
        ));
    }
    let mut pin = WinPin {
        hwnd,
        _surface: match Surface::new(frame) {
            Ok(s) => s,
            Err(e) => {
                // SAFETY: the window created above.
                unsafe { DestroyWindow(hwnd) };
                return Err(e);
            }
        },
    };
    // SAFETY: our window; the id is read back by the window procedure.
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, isize::try_from(id.0).unwrap_or(0));
        // No input method context: letter keys are shortcuts, also in Hangul mode.
        ImmAssociateContextEx(hwnd, std::ptr::null_mut(), 0);
    }
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    let pos = POINT { x: r.x, y: r.y };
    let size = SIZE { cx: w, cy: h };
    let src = POINT { x: 0, y: 0 };
    // SAFETY: the screen DC is released right after; our surface DC holds the premultiplied
    // picture of exactly `size`.
    let ok = unsafe {
        let screen = GetDC(std::ptr::null_mut());
        let ok = UpdateLayeredWindow(
            hwnd,
            screen,
            &raw const pos,
            &raw const size,
            pin._surface.dc,
            &raw const src,
            0,
            &raw const blend,
            ULW_ALPHA,
        );
        ReleaseDC(std::ptr::null_mut(), screen);
        ok
    };
    if ok == 0 {
        let e = format!(
            "UpdateLayeredWindow failed ({})",
            std::io::Error::last_os_error()
        );
        pin.destroy();
        return Err(e);
    }
    // SAFETY: shows our window; activation only when the user created it.
    unsafe {
        ShowWindow(hwnd, if focus { SW_SHOW } else { SW_SHOWNOACTIVATE });
    }
    if focus {
        pin.focus();
    }
    Ok(pin)
}

impl PinWindows for WinPins {
    fn create(
        &mut self,
        id: PinId,
        frame: &Frame,
        focus: bool,
    ) -> Result<Box<dyn PinWindow>, String> {
        create(id, frame, focus).map(|p| Box::new(p) as Box<dyn PinWindow>)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{Ledger, Rect};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetForegroundWindow, GetWindowLongW, IsWindowVisible, SendMessageW,
        WS_EX_APPWINDOW,
    };

    /// A real pin window, created off-screen (-6000,-6000), never activated: the user sees
    /// nothing. Checks styles, focus, key handling, double-click and destruction.
    #[test]
    fn off_screen_pin_window_lifecycle() {
        let ledger = Ledger::new();
        let frame =
            Frame::solid(&ledger, Rect::new(-6000, -6000, 16, 12), [0, 128, 255, 255]).unwrap();
        let mut pin = create(PinId(77), &frame, false).unwrap();
        let hwnd = pin.hwnd();
        // SAFETY: queries on our own live window.
        unsafe {
            assert_ne!(IsWindow(hwnd), 0);
            assert_ne!(IsWindowVisible(hwnd), 0);
            let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
            assert_ne!(ex & WS_EX_TOOLWINDOW, 0, "no taskbar entry, not in Alt+Tab");
            assert_ne!(ex & WS_EX_TOPMOST, 0, "always on top");
            assert_ne!(ex & WS_EX_LAYERED, 0);
            assert_eq!(ex & WS_EX_APPWINDOW, 0);
            assert_ne!(GetForegroundWindow(), hwnd, "a CLI pin never steals focus");
            let mut r = RECT::default();
            GetWindowRect(hwnd, &raw mut r);
            assert_eq!(
                (r.left, r.top, r.right - r.left, r.bottom - r.top),
                (-6000, -6000, 16, 12)
            );
        }
        let _ = take_events();
        // SAFETY: synchronous messages to our own window on this thread.
        unsafe {
            SendMessageW(hwnd, WM_KEYDOWN, 0x1B, 0); // Escape
            SendMessageW(hwnd, WM_NCLBUTTONDBLCLK, HTCAPTION as usize, 0);
            SendMessageW(hwnd, WM_KEYDOWN, 0x25, 1 << 30); // Left, auto-repeat
            SendMessageW(hwnd, WM_KEYDOWN, 0xFF, 0); // no key name
            SendMessageW(hwnd, WM_CLOSE, 0, 0);
            assert_ne!(
                IsWindow(hwnd),
                0,
                "WM_CLOSE leaves the decision to the core"
            );
            assert_eq!(SendMessageW(hwnd, WM_NCHITTEST, 0, 0), HTCAPTION as LRESULT);
        }
        let ev = take_events();
        let esc = chord_of("Escape", false, false, false).unwrap();
        let left = chord_of("Left", false, false, false).unwrap();
        assert_eq!(
            ev,
            [
                PinEvent::Key(PinId(77), esc, false),
                PinEvent::Key(PinId(77), left, true),
                PinEvent::Closed(PinId(77)),
            ],
            "the double-click produced nothing (NOEUL-FR-026)"
        );
        pin.set_position(-6100, -6050);
        // SAFETY: as above.
        unsafe {
            let mut r = RECT::default();
            GetWindowRect(hwnd, &raw mut r);
            assert_eq!((r.left, r.top), (-6100, -6050));
        }
        pin.destroy();
        // SAFETY: as above.
        assert_eq!(unsafe { IsWindow(hwnd) }, 0, "destroyed");
        pin.destroy(); // idempotent
        drop(frame);
        assert_eq!(ledger.live(), (0, 0));
    }

    #[test]
    fn key_names_follow_the_contract() {
        assert_eq!(vk_name(0x41).as_deref(), Some("A"));
        assert_eq!(vk_name(0x37).as_deref(), Some("7"));
        assert_eq!(vk_name(0x70).as_deref(), Some("F1"));
        assert_eq!(vk_name(0x87).as_deref(), Some("F24"));
        assert_eq!(vk_name(0x1B).as_deref(), Some("Escape"));
        assert_eq!(vk_name(0x60).as_deref(), Some("Numpad0"));
        assert_eq!(vk_name(0x5B), None, "Win keys are not names");
        for vk in (0x08..=0xFE).filter_map(vk_name) {
            assert!(chord_of(&vk, false, false, false).is_some(), "{vk} parses");
        }
    }
}
