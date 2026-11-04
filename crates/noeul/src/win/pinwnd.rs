//! Win32 pin windows: borderless, always-on-top, layered tool windows (NOEUL-FR-060,
//! NOEUL-FR-061, NOEUL-FR-026; ADR 0023 §2, ADR 0029).
//!
//! - **Never on the taskbar or in Alt+Tab** (`WS_EX_TOOLWINDOW`, no `WS_EX_APPWINDOW`).
//! - **Retained picture.** The window's picture is rendered once per change ([`zoom::render`]:
//!   at 100 % one image pixel per device pixel) into a temporary premultiplied DIB section and
//!   handed to `UpdateLayeredWindow`, which keeps its own copy; the DIB is freed at once. No
//!   `WM_PAINT`, no timer, no GPU context: an idle pin costs no CPU (NOEUL-NFR-001), and its
//!   memory is measured in ADR 0029 (NOEUL-NFR-002).
//! - **Focus only when the user asks.** Created with `SW_SHOWNOACTIVATE` unless the user made it
//!   (then it takes focus so `Esc` and the arrows work at once); a click activates it. Hiding or
//!   closing the focused pin first hands focus back to the window the user was in before the
//!   pin took it (never to another pin), so the next key reaches that program, not a pin.
//!   Showing, moving and zooming never activate anything. A window created for the debug-build
//!   test hook ([`Style::OFF_SCREEN_TEST`]) is never topmost and can never be activated.
//! - **Z-order.** A new pin, and a pin shown again, go to the top of the topmost band once;
//!   pins never fight other topmost windows for it (no timer, no re-assertion). The capture
//!   overlay, created later, lies above them.
//! - **Input stays Noeul's own**: keys reach the pin only while it has focus; they are matched
//!   against the `pin` context and never forwarded anywhere (NOEUL-SEC-012). The IME is switched
//!   off for the window so letter shortcuts work in Hangul mode (NOEUL-NFR-014).
//! - **Mouse.** Dragging anywhere moves it (the system move loop, `HTCAPTION`); the wheel zooms
//!   around the pointer; a middle-click returns to 100 %; a right-click (or `Shift+F10`, or the
//!   menu key) opens its context menu. A double-click does nothing (NOEUL-FR-026). For the guard
//!   time a new pin was given, every press on it is swallowed (UX spec §4.2 rule 10).
//! - **Displays.** `WM_DPICHANGED` is ignored: a pin keeps its pixel size on every monitor
//!   (NOEUL-FR-061, NOEUL-FR-091).
//!
//! The window procedure only queues [`PinEvent`]s and calls the registered waker; the instance
//! core handles them later, never re-entrantly. The context menu is shown by [`track_menu`],
//! which the runtime calls with the core released.

use crate::app::{PinEvent, PinMenuItem};
use crate::frame::{Frame, Rect};
use crate::hotkeys::chord_of;
use crate::pins::{PinId, PinWindow, PinWindows, WindowSpec};
use crate::win::wide;
use crate::zoom;
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::ffi::c_void;
use std::sync::OnceLock;
use std::time::Instant;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, HBITMAP,
    HDC, ReleaseDC, SelectObject,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::SystemServices::MK_CONTROL;
use windows_sys::Win32::UI::Input::Ime::ImmAssociateContextEx;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_MENU, VK_SHIFT};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CS_DBLCLKS, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
    DestroyWindow, GWLP_USERDATA, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, HTCAPTION,
    HWND_TOP, HWND_TOPMOST, IDC_SIZEALL, IsWindow, IsWindowVisible, LoadCursorW, MA_ACTIVATE,
    MA_NOACTIVATEANDEAT, MF_GRAYED, MF_SEPARATOR, MF_STRING, PostMessageW, RegisterClassExW,
    SW_HIDE, SW_SHOW, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    SWP_SHOWWINDOW, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, ShowWindow, TPM_NONOTIFY,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, ULW_ALPHA, UpdateLayeredWindow, WA_INACTIVE,
    WM_ACTIVATE, WM_CLOSE, WM_CONTEXTMENU, WM_DESTROY, WM_DPICHANGED, WM_EXITSIZEMOVE, WM_KEYDOWN,
    WM_LBUTTONDOWN, WM_MBUTTONDOWN, WM_MOUSEACTIVATE, WM_MOUSEWHEEL, WM_NCDESTROY, WM_NCHITTEST,
    WM_NCLBUTTONDBLCLK, WM_NCLBUTTONDOWN, WM_NCLBUTTONUP, WM_NCMBUTTONDBLCLK, WM_NCMBUTTONDOWN,
    WM_NCMBUTTONUP, WM_NCRBUTTONDBLCLK, WM_NCRBUTTONDOWN, WM_NCRBUTTONUP, WM_NULL, WM_RBUTTONDOWN,
    WM_SYSKEYDOWN, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_POPUP,
};

const CLASS: &str = "NoeulPin";

/// What the runtime knows about one live pin window.
#[derive(Debug, Clone, Copy)]
struct Slot {
    id: PinId,
    /// The window the user was in before this pin took focus (never a pin); 0 when unknown.
    prev: isize,
    /// Presses are swallowed until then (the rest of the gesture that created the pin).
    guard_until: Option<Instant>,
    /// How often the window was activated (tests check that nothing activated it).
    activations: u32,
}

thread_local! {
    static EVENTS: RefCell<VecDeque<PinEvent>> = const { RefCell::new(VecDeque::new()) };
    static WAKER: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
    static SLOTS: RefCell<BTreeMap<isize, Slot>> = const { RefCell::new(BTreeMap::new()) };
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

fn slot(hwnd: HWND) -> Option<Slot> {
    SLOTS.with(|s| s.borrow().get(&(hwnd as isize)).copied())
}

fn with_slot(hwnd: HWND, f: impl FnOnce(&mut Slot)) {
    SLOTS.with(|s| {
        if let Ok(mut s) = s.try_borrow_mut()
            && let Some(slot) = s.get_mut(&(hwnd as isize))
        {
            f(slot);
        }
    });
}

/// The window of an open pin on this thread.
pub fn hwnd_of(id: PinId) -> Option<HWND> {
    SLOTS.with(|s| {
        s.borrow()
            .iter()
            .find(|(_, slot)| slot.id == id)
            .map(|(h, _)| *h as HWND)
    })
}

/// How often the pin window was activated since it was created.
pub fn activations(hwnd: HWND) -> Option<u32> {
    slot(hwnd).map(|s| s.activations)
}

fn is_pin(hwnd: HWND) -> bool {
    slot(hwnd).is_some()
}

/// The window to give focus back to when `hwnd` (about to be activated) takes it from
/// `from`: `from` itself, or, when `from` is another pin, the window that pin got it from.
fn origin_of(from: HWND) -> isize {
    if from.is_null() {
        return 0;
    }
    match slot(from) {
        Some(s) => s.prev,
        None => from as isize,
    }
}

fn guarded(hwnd: HWND) -> bool {
    slot(hwnd)
        .and_then(|s| s.guard_until)
        .is_some_and(|t| Instant::now() < t)
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

/// Screen coordinates packed in a mouse message's `lParam`.
fn point_of(lp: LPARAM) -> (i32, i32) {
    let x = i32::from((lp & 0xFFFF) as u16 as i16);
    let y = i32::from(((lp >> 16) & 0xFFFF) as u16 as i16);
    (x, y)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        // Drag anywhere to move.
        WM_NCHITTEST => return HTCAPTION as LRESULT,
        // The rest of the creating gesture is swallowed: no activation, no move, no menu.
        WM_MOUSEACTIVATE => {
            if guarded(hwnd) {
                return MA_NOACTIVATEANDEAT as LRESULT;
            }
            // SAFETY: plain query; the window that has focus before this click activates us.
            let before = unsafe { GetForegroundWindow() };
            if before != hwnd {
                let prev = origin_of(before);
                with_slot(hwnd, |s| s.prev = prev);
            }
            return MA_ACTIVATE as LRESULT;
        }
        WM_ACTIVATE => {
            if (wp & 0xFFFF) as u32 != WA_INACTIVE {
                let from = lp as HWND;
                with_slot(hwnd, |s| s.activations += 1);
                // The window being deactivated (null when it belongs to another thread).
                if !from.is_null() && from != hwnd {
                    let prev = origin_of(from);
                    with_slot(hwnd, |s| s.prev = prev);
                }
            }
        }
        WM_NCLBUTTONDOWN | WM_NCRBUTTONDOWN | WM_NCMBUTTONDOWN | WM_NCLBUTTONUP
        | WM_NCRBUTTONUP | WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN
            if guarded(hwnd) =>
        {
            return 0;
        }
        // A double-click on a pin has no default action (NOEUL-FR-026); never maximize.
        WM_NCLBUTTONDBLCLK | WM_NCRBUTTONDBLCLK | WM_NCMBUTTONDBLCLK => return 0,
        WM_NCMBUTTONUP => {
            if let Some(id) = pin_of(hwnd)
                && !guarded(hwnd)
            {
                push(PinEvent::MiddleClick(id));
            }
            return 0;
        }
        WM_CONTEXTMENU => {
            if let Some(id) = pin_of(hwnd)
                && !guarded(hwnd)
            {
                // -1 (both words 0xFFFF) means the keyboard asked (the menu key).
                let at = (lp as u32 != u32::MAX).then(|| point_of(lp));
                push(PinEvent::Menu(id, at));
            }
            return 0;
        }
        WM_MOUSEWHEEL => {
            if let Some(id) = pin_of(hwnd) {
                let delta = i32::from(((wp >> 16) & 0xFFFF) as u16 as i16);
                let ctrl = (wp & 0xFFFF) as u32 & MK_CONTROL != 0;
                push(PinEvent::Wheel(id, delta, point_of(lp), ctrl));
            }
            return 0;
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let vk = u32::try_from(wp).unwrap_or(0);
            let repeat = (lp >> 30) & 1 == 1;
            let (ctrl, alt, shift) = (down(VK_CONTROL), down(VK_MENU), down(VK_SHIFT));
            if let (Some(id), Some(name)) = (pin_of(hwnd), vk_name(vk)) {
                // Shift+F10 opens the context menu (UX spec §5).
                if name == "F10" && shift && !ctrl && !alt {
                    if !repeat {
                        push(PinEvent::Menu(id, None));
                    }
                    return 0;
                }
                // Alt+F4 stays the system's (reserved `window.close`): it arrives as WM_CLOSE.
                if let Some(chord) = chord_of(&name, ctrl, alt, shift)
                    && !(msg == WM_SYSKEYDOWN && name == "F4")
                {
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
        // One image pixel per device pixel on every monitor: the suggested size is ignored.
        WM_DPICHANGED => return 0,
        WM_CLOSE => {
            // Alt+F4 or the system: the core closes the pin (and destroys this window).
            if let Some(id) = pin_of(hwnd) {
                push(PinEvent::Closed(id));
            }
            return 0;
        }
        WM_DESTROY => return 0,
        WM_NCDESTROY => {
            SLOTS.with(|s| {
                if let Ok(mut s) = s.try_borrow_mut() {
                    s.remove(&(hwnd as isize));
                }
            });
        }
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

/// A temporary premultiplied BGRA DIB section of one window size.
struct Surface {
    dc: HDC,
    bitmap: HBITMAP,
    old: *mut c_void,
    bits: *mut u8,
    len: usize,
}

impl Surface {
    fn new(w: u32, h: u32) -> Result<Surface, String> {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: u32::try_from(size_of::<BITMAPINFOHEADER>()).expect("small"),
                biWidth: i32::try_from(w).map_err(|_| "too wide")?,
                biHeight: -i32::try_from(h).map_err(|_| "too high")?,
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
        // SAFETY: `info` describes a top-down 32-bit DIB of `w`×`h`.
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
        // SAFETY: selects our bitmap; restored in Drop.
        let old = unsafe { SelectObject(dc, bitmap) };
        Ok(Surface {
            dc,
            bitmap,
            old,
            bits: bits.cast::<u8>(),
            len: usize::try_from(u64::from(w) * u64::from(h) * 4).map_err(|_| "too large")?,
        })
    }

    fn pixels(&mut self) -> &mut [u8] {
        // SAFETY: the DIB holds `len` bytes at `bits` until Drop; `&mut self` makes this the
        // only view.
        unsafe { std::slice::from_raw_parts_mut(self.bits, self.len) }
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

/// Renders `frame` at `rect`'s size and hands it to the layered window, which moves and resizes
/// to `rect` (no activation, no z-order change). The temporary surface is freed on return.
fn upload(hwnd: HWND, frame: &Frame, rect: Rect) -> Result<(), String> {
    if !zoom::allowed(rect.width, rect.height) {
        return Err(format!(
            "a {}×{} pin window is too large",
            rect.width, rect.height
        ));
    }
    let mut surface = Surface::new(rect.width, rect.height)?;
    zoom::render(frame, rect.width, rect.height, surface.pixels());
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    let pos = POINT {
        x: rect.x,
        y: rect.y,
    };
    let size = SIZE {
        cx: i32::try_from(rect.width).map_err(|_| "too wide")?,
        cy: i32::try_from(rect.height).map_err(|_| "too high")?,
    };
    let src = POINT { x: 0, y: 0 };
    // SAFETY: the screen DC is released right after; the surface DC holds the premultiplied
    // picture of exactly `size`.
    let ok = unsafe {
        let screen = GetDC(std::ptr::null_mut());
        let ok = UpdateLayeredWindow(
            hwnd,
            screen,
            &raw const pos,
            &raw const size,
            surface.dc,
            &raw const src,
            0,
            &raw const blend,
            ULW_ALPHA,
        );
        ReleaseDC(std::ptr::null_mut(), screen);
        ok
    };
    if ok == 0 {
        return Err(format!(
            "UpdateLayeredWindow failed ({})",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// How pin windows sit on the desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    /// Above every normal window (`WS_EX_TOPMOST`).
    pub topmost: bool,
    /// May be activated (by the user's click or creation); `false` adds `WS_EX_NOACTIVATE`.
    pub activate: bool,
}

impl Style {
    /// Real pins.
    pub const INTERACTIVE: Style = Style {
        topmost: true,
        activate: true,
    };
    /// The debug-build test hook (ADR 0026 §8): the windows lie off-screen, never on top, never
    /// activated.
    pub const OFF_SCREEN_TEST: Style = Style {
        topmost: false,
        activate: false,
    };
}

/// One live pin window.
pub struct WinPin {
    hwnd: HWND,
    style: Style,
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

    /// When this pin has focus, gives it back to the window the user was in before (if that
    /// still exists and is visible), so hiding or closing a pin never moves focus to another pin.
    fn hand_back_focus(&self) {
        // SAFETY: plain queries and an activation this process may make while it has focus.
        unsafe {
            if self.hwnd.is_null() || GetForegroundWindow() != self.hwnd {
                return;
            }
            let prev = slot(self.hwnd).map_or(0, |s| s.prev) as HWND;
            if !prev.is_null() && IsWindow(prev) != 0 && IsWindowVisible(prev) != 0 && !is_pin(prev)
            {
                SetForegroundWindow(prev);
            }
        }
    }
}

impl PinWindow for WinPin {
    fn set_position(&mut self, x: i32, y: i32) {
        // SAFETY: our window; moves it without activating, resizing or changing the z-order.
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

    fn present(&mut self, frame: &Frame, rect: Rect) -> Result<(), String> {
        upload(self.hwnd, frame, rect)
    }

    fn set_visible(&mut self, visible: bool) {
        if visible {
            let top = if self.style.topmost {
                HWND_TOPMOST
            } else {
                HWND_TOP
            };
            // SAFETY: our window; shown at the top of its band without activation.
            unsafe {
                SetWindowPos(
                    self.hwnd,
                    top,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                )
            };
        } else {
            self.hand_back_focus();
            // SAFETY: our window; it is no longer the active one, so hiding moves no focus.
            unsafe { ShowWindow(self.hwnd, SW_HIDE) };
        }
    }

    fn focus(&mut self) -> bool {
        if !self.style.activate {
            return false;
        }
        // SAFETY: plain query, then our window; the OS may refuse (foreground lock), which is
        // reported.
        unsafe {
            let before = GetForegroundWindow();
            if before != self.hwnd {
                let prev = origin_of(before);
                with_slot(self.hwnd, |s| s.prev = prev);
            }
            SetForegroundWindow(self.hwnd) != 0
        }
    }

    fn rect(&self) -> Option<Rect> {
        let mut r = RECT::default();
        // SAFETY: our window; `r` is writable.
        if self.hwnd.is_null() || unsafe { GetWindowRect(self.hwnd, &raw mut r) } == 0 {
            return None;
        }
        let w = u32::try_from(r.right - r.left).ok()?;
        let h = u32::try_from(r.bottom - r.top).ok()?;
        Some(Rect::new(r.left, r.top, w, h))
    }

    fn destroy(&mut self) {
        self.hand_back_focus();
        // SAFETY: destroys our own window once; later calls see a dead handle and do nothing.
        unsafe {
            if !self.hwnd.is_null() && IsWindow(self.hwnd) != 0 {
                SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
                DestroyWindow(self.hwnd);
            }
        }
        SLOTS.with(|s| {
            if let Ok(mut s) = s.try_borrow_mut() {
                s.remove(&(self.hwnd as isize));
            }
        });
        self.hwnd = std::ptr::null_mut();
    }
}

impl Drop for WinPin {
    fn drop(&mut self) {
        self.destroy();
    }
}

/// Creates Win32 pin windows on the calling (UI) thread.
#[derive(Debug, Clone, Copy)]
pub struct WinPins {
    pub style: Style,
}

/// Creates one pin window showing `frame` as `spec` says. Exposed for tests and measurements,
/// which place it off-screen.
pub fn create(id: PinId, frame: &Frame, spec: &WindowSpec, style: Style) -> Result<WinPin, String> {
    let class = class()?;
    let r = spec.rect;
    if !zoom::allowed(r.width, r.height) {
        return Err(format!(
            "a {}×{} pin window is too large",
            r.width, r.height
        ));
    }
    let mut ex = WS_EX_LAYERED | WS_EX_TOOLWINDOW;
    if style.topmost {
        ex |= WS_EX_TOPMOST;
    }
    if !style.activate {
        ex |= WS_EX_NOACTIVATE;
    }
    let title = wide(format!("Noeul {id}"));
    // SAFETY: registered class; plain values; no parent, menu or creation data.
    let hwnd = unsafe {
        CreateWindowExW(
            ex,
            class,
            title.as_ptr(),
            WS_POPUP,
            r.x,
            r.y,
            i32::try_from(r.width).map_err(|_| "too wide")?,
            i32::try_from(r.height).map_err(|_| "too high")?,
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
    SLOTS.with(|s| {
        s.borrow_mut().insert(
            hwnd as isize,
            Slot {
                id,
                prev: 0,
                guard_until: spec.guard.map(|g| Instant::now() + g),
                activations: 0,
            },
        );
    });
    let mut pin = WinPin { hwnd, style };
    // SAFETY: our window; the id is read back by the window procedure.
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, isize::try_from(id.0).unwrap_or(0));
        // No input method context: letter keys are shortcuts, also in Hangul mode.
        ImmAssociateContextEx(hwnd, std::ptr::null_mut(), 0);
    }
    if let Err(e) = upload(hwnd, frame, r) {
        pin.destroy();
        return Err(e);
    }
    let focus = spec.focus && style.activate;
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
        spec: &WindowSpec,
    ) -> Result<Box<dyn PinWindow>, String> {
        create(id, frame, spec, self.style).map(|p| Box::new(p) as Box<dyn PinWindow>)
    }
}

/// Shows the context menu of pin `id` (modal; call it while the instance core is not borrowed)
/// at `at`, or at the pin's top-left corner when the keyboard asked, and returns the chosen
/// action. `None` when the menu was dismissed or the pin is gone.
pub fn track_menu(
    id: PinId,
    items: &[PinMenuItem],
    at: Option<(i32, i32)>,
) -> Option<&'static str> {
    let hwnd = hwnd_of(id)?;
    // SAFETY: a new popup menu, destroyed below.
    let menu = unsafe { CreatePopupMenu() };
    if menu.is_null() {
        return None;
    }
    for (i, item) in items.iter().enumerate() {
        if item.action.is_none() {
            // SAFETY: valid menu handle.
            unsafe { AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null()) };
            continue;
        }
        let label = wide(&item.label);
        let flags = if item.enabled {
            MF_STRING
        } else {
            MF_STRING | MF_GRAYED
        };
        // SAFETY: valid menu; the label is copied.
        unsafe { AppendMenuW(menu, flags, i + 1, label.as_ptr()) };
    }
    let (x, y) = at.unwrap_or_else(|| {
        let mut r = RECT::default();
        // SAFETY: our window; `r` is writable.
        unsafe { GetWindowRect(hwnd, &raw mut r) };
        (r.left.saturating_add(8), r.top.saturating_add(8))
    });
    // SAFETY: the documented popup-menu pattern: the owner is foreground while the menu is open
    // (the pin already is after the click or key that asked), and a WM_NULL afterwards lets the
    // menu close correctly.
    let cmd = unsafe {
        SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
            x,
            y,
            0,
            hwnd,
            std::ptr::null(),
        );
        PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(menu);
        cmd
    };
    let index = usize::try_from(cmd).ok()?.checked_sub(1)?;
    items.get(index).and_then(|i| i.action)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Ledger;
    use std::time::Duration;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GWL_EXSTYLE, GetWindowLongW, SendMessageW, WS_EX_APPWINDOW,
    };

    /// Off-screen: far outside any real monitor arrangement (the user sees nothing).
    const FAR: i32 = -20_000;

    fn spec(rect: Rect) -> WindowSpec {
        WindowSpec {
            rect,
            focus: false,
            guard: None,
        }
    }

    /// Off-screen and never activated, but on top: z-order checks need real topmost windows,
    /// and these lie where no monitor shows them.
    const TOPMOST_TEST: Style = Style {
        topmost: true,
        activate: false,
    };

    fn ex_style(hwnd: HWND) -> u32 {
        // SAFETY: a query on our own window.
        unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 }
    }

    fn rect_of(hwnd: HWND) -> (i32, i32, i32, i32) {
        let mut r = RECT::default();
        // SAFETY: a query on our own window; `r` is writable.
        unsafe { GetWindowRect(hwnd, &raw mut r) };
        (r.left, r.top, r.right - r.left, r.bottom - r.top)
    }

    /// Top-level windows from the top of the z-order down.
    fn z_order() -> Vec<isize> {
        unsafe extern "system" fn each(h: HWND, data: LPARAM) -> i32 {
            // SAFETY: `data` is the `&mut Vec<isize>` passed below, alive during EnumWindows.
            unsafe { (*(data as *mut Vec<isize>)).push(h as isize) };
            1
        }
        let mut out: Vec<isize> = Vec::new();
        // SAFETY: the callback only touches `out`, passed by pointer for the duration.
        unsafe { EnumWindows(Some(each), (&raw mut out) as LPARAM) };
        out
    }

    /// A real pin window, created off-screen, never activated: the user sees nothing. Checks
    /// styles, focus, key handling, double-click, the menu request, DPI changes and destruction.
    #[test]
    fn off_screen_pin_window_lifecycle() {
        let ledger = Ledger::new();
        let frame = Frame::solid(&ledger, Rect::new(FAR, FAR, 16, 12), [0, 128, 255, 255]).unwrap();
        let mut pin = create(PinId(77), &frame, &spec(frame.rect()), Style::INTERACTIVE).unwrap();
        let hwnd = pin.hwnd();
        assert_eq!(hwnd_of(PinId(77)), Some(hwnd));
        // SAFETY: queries on our own live window.
        unsafe {
            assert_ne!(IsWindow(hwnd), 0);
            assert_ne!(IsWindowVisible(hwnd), 0);
            let ex = ex_style(hwnd);
            assert_ne!(ex & WS_EX_TOOLWINDOW, 0, "no taskbar entry, not in Alt+Tab");
            assert_ne!(ex & WS_EX_TOPMOST, 0, "always on top");
            assert_ne!(ex & WS_EX_LAYERED, 0);
            assert_eq!(ex & WS_EX_APPWINDOW, 0);
            assert_eq!(
                ex & WS_EX_NOACTIVATE,
                0,
                "a click may give it focus (NOEUL-FR-060)"
            );
            assert_ne!(GetForegroundWindow(), hwnd, "a CLI pin never steals focus");
        }
        assert_eq!(rect_of(hwnd), (FAR, FAR, 16, 12));
        assert_eq!(activations(hwnd), Some(0));
        let _ = take_events();
        // SAFETY: synchronous messages to our own window on this thread.
        unsafe {
            SendMessageW(hwnd, WM_KEYDOWN, 0x1B, 0); // Escape
            SendMessageW(hwnd, WM_NCLBUTTONDBLCLK, HTCAPTION as usize, 0);
            SendMessageW(hwnd, WM_KEYDOWN, 0x25, 1 << 30); // Left, auto-repeat
            SendMessageW(hwnd, WM_KEYDOWN, 0xFF, 0); // no key name
            SendMessageW(hwnd, WM_CONTEXTMENU, hwnd as usize, -1); // the menu key
            let at = ((FAR as u16 as isize) & 0xFFFF) | ((FAR + 3) as u16 as isize) << 16;
            SendMessageW(hwnd, WM_CONTEXTMENU, hwnd as usize, at); // a right-click
            SendMessageW(hwnd, WM_NCMBUTTONUP, HTCAPTION as usize, 0);
            SendMessageW(
                hwnd,
                WM_MOUSEWHEEL,
                (120usize << 16) | MK_CONTROL as usize,
                0,
            );
            SendMessageW(hwnd, WM_MOUSEWHEEL, (0xFF88usize) << 16, 0); // -120
            SendMessageW(hwnd, WM_CLOSE, 0, 0);
            assert_ne!(
                IsWindow(hwnd),
                0,
                "WM_CLOSE leaves the decision to the core"
            );
            assert_eq!(SendMessageW(hwnd, WM_NCHITTEST, 0, 0), HTCAPTION as LRESULT);
            // A DPI change keeps the pixel size.
            let suggested = RECT {
                left: FAR,
                top: FAR,
                right: FAR + 24,
                bottom: FAR + 18,
            };
            SendMessageW(
                hwnd,
                WM_DPICHANGED,
                (144 << 16) | 144,
                (&raw const suggested) as LPARAM,
            );
        }
        assert_eq!(
            rect_of(hwnd),
            (FAR, FAR, 16, 12),
            "WM_DPICHANGED changes nothing"
        );
        let ev = take_events();
        let esc = chord_of("Escape", false, false, false).unwrap();
        let left = chord_of("Left", false, false, false).unwrap();
        assert_eq!(
            ev,
            [
                PinEvent::Key(PinId(77), esc, false),
                PinEvent::Key(PinId(77), left, true),
                PinEvent::Menu(PinId(77), None),
                PinEvent::Menu(PinId(77), Some((FAR, FAR + 3))),
                PinEvent::MiddleClick(PinId(77)),
                PinEvent::Wheel(PinId(77), 120, (0, 0), true),
                PinEvent::Wheel(PinId(77), -120, (0, 0), false),
                PinEvent::Closed(PinId(77)),
            ],
            "the double-click produced nothing (NOEUL-FR-026)"
        );
        pin.set_position(FAR - 100, FAR - 50);
        assert_eq!(rect_of(hwnd).0, FAR - 100);
        assert_eq!(pin.rect(), Some(Rect::new(FAR - 100, FAR - 50, 16, 12)));
        // Zoom: the window takes the new size and place, still not activated.
        pin.present(&frame, Rect::new(FAR, FAR, 32, 24)).unwrap();
        assert_eq!(rect_of(hwnd), (FAR, FAR, 32, 24));
        assert!(
            pin.present(&frame, Rect::new(FAR, FAR, 20_000, 1)).is_err(),
            "too large"
        );
        assert_eq!(activations(hwnd), Some(0), "nothing activated it");
        pin.destroy();
        // SAFETY: as above.
        assert_eq!(unsafe { IsWindow(hwnd) }, 0, "destroyed");
        assert_eq!(hwnd_of(PinId(77)), None);
        pin.destroy(); // idempotent
        drop(frame);
        assert_eq!(ledger.live(), (0, 0));
    }

    /// The tail of a double-click: for the guard time every press is swallowed and the window
    /// refuses activation; afterwards a click may activate it again.
    #[test]
    fn a_new_pin_swallows_the_rest_of_the_gesture() {
        let ledger = Ledger::new();
        let frame = Frame::solid(&ledger, Rect::new(FAR, FAR, 8, 8), [1, 2, 3, 255]).unwrap();
        let guarded_spec = WindowSpec {
            guard: Some(Duration::from_millis(300)),
            ..spec(frame.rect())
        };
        let pin = create(PinId(91), &frame, &guarded_spec, Style::INTERACTIVE).unwrap();
        let hwnd = pin.hwnd();
        let _ = take_events();
        // SAFETY: synchronous messages to our own window on this thread.
        unsafe {
            assert_eq!(
                SendMessageW(hwnd, WM_MOUSEACTIVATE, hwnd as usize, 0),
                MA_NOACTIVATEANDEAT as LRESULT,
                "the third click of a triple click neither activates nor reaches the pin"
            );
            assert_eq!(
                SendMessageW(hwnd, WM_NCLBUTTONDOWN, HTCAPTION as usize, 0),
                0
            );
            SendMessageW(hwnd, WM_NCMBUTTONUP, HTCAPTION as usize, 0);
            SendMessageW(hwnd, WM_CONTEXTMENU, hwnd as usize, -1);
        }
        assert!(take_events().is_empty(), "nothing reached the core");
        std::thread::sleep(Duration::from_millis(350));
        // SAFETY: as above.
        unsafe {
            assert_eq!(
                SendMessageW(hwnd, WM_MOUSEACTIVATE, hwnd as usize, 0),
                MA_ACTIVATE as LRESULT,
                "after the guard a click may activate it"
            );
            SendMessageW(hwnd, WM_NCMBUTTONUP, HTCAPTION as usize, 0);
        }
        assert_eq!(take_events(), [PinEvent::MiddleClick(PinId(91))]);
        drop(pin);
    }

    /// Several real topmost pins, off-screen: each is independent, the newest is on top, hiding
    /// and showing one leaves the others alone, a shown one returns to the top of the topmost
    /// band, and no window is ever activated or made the foreground window.
    #[test]
    fn independent_topmost_pins_keep_their_z_order_and_never_take_focus() {
        let ledger = Ledger::new();
        // SAFETY: a plain query.
        let fg_before = unsafe { GetForegroundWindow() };
        let frames: Vec<Frame> = (0..3)
            .map(|i| {
                Frame::solid(
                    &ledger,
                    Rect::new(FAR + i * 40, FAR, 30, 20),
                    [i as u8, 0, 0, 255],
                )
                .unwrap()
            })
            .collect();
        let mut pins: Vec<WinPin> = frames
            .iter()
            .enumerate()
            .map(|(i, f)| create(PinId(200 + i as u64), f, &spec(f.rect()), TOPMOST_TEST).unwrap())
            .collect();
        let h: Vec<HWND> = pins.iter().map(WinPin::hwnd).collect();
        // A normal (not topmost) window of ours, also off-screen.
        let normal_frame =
            Frame::solid(&ledger, Rect::new(FAR, FAR + 100, 10, 10), [0; 4]).unwrap();
        let normal = create(
            PinId(299),
            &normal_frame,
            &spec(normal_frame.rect()),
            Style::OFF_SCREEN_TEST,
        )
        .unwrap();
        let pos = |order: &[isize], w: HWND| order.iter().position(|x| *x == w as isize).unwrap();
        let z = z_order();
        assert!(
            pos(&z, h[2]) < pos(&z, h[1]) && pos(&z, h[1]) < pos(&z, h[0]),
            "newest on top"
        );
        assert!(
            pos(&z, h[0]) < pos(&z, normal.hwnd()),
            "pins lie above normal windows"
        );
        assert_eq!(ex_style(normal.hwnd()) & WS_EX_TOPMOST, 0);
        assert_ne!(
            ex_style(normal.hwnd()) & WS_EX_NOACTIVATE,
            0,
            "the test style never activates"
        );
        // Hide the oldest: only it disappears.
        pins[0].set_visible(false);
        // SAFETY: queries on our own windows.
        unsafe {
            assert_eq!(IsWindowVisible(h[0]), 0);
            assert_ne!(IsWindowVisible(h[1]), 0);
            assert_ne!(IsWindowVisible(h[2]), 0);
        }
        // Show it again: it comes back on top of the other pins, without activation.
        pins[0].set_visible(true);
        let z = z_order();
        assert!(
            pos(&z, h[0]) < pos(&z, h[2]),
            "a shown pin returns to the top"
        );
        assert_ne!(ex_style(h[0]) & WS_EX_TOPMOST, 0, "still topmost");
        // Move and zoom the middle one; the others keep their rectangles.
        pins[1].set_position(FAR + 500, FAR + 7);
        pins[1]
            .present(&frames[1], Rect::new(FAR + 500, FAR + 7, 60, 40))
            .unwrap();
        assert_eq!(rect_of(h[1]), (FAR + 500, FAR + 7, 60, 40));
        assert_eq!(rect_of(h[0]), (FAR, FAR, 30, 20));
        assert_eq!(rect_of(h[2]), (FAR + 80, FAR, 30, 20));
        // Close the middle one: the others stay alive and visible.
        pins[1].destroy();
        // SAFETY: queries on our own windows.
        unsafe {
            assert_eq!(IsWindow(h[1]), 0);
            assert_ne!(IsWindowVisible(h[0]), 0);
            assert_ne!(IsWindowVisible(h[2]), 0);
        }
        for w in [h[0], h[2]] {
            assert_eq!(activations(w), Some(0), "never activated");
        }
        // SAFETY: a plain query.
        let fg_after = unsafe { GetForegroundWindow() };
        assert!(
            !h.contains(&fg_after) && fg_after != normal.hwnd(),
            "no pin became the foreground window"
        );
        if fg_after != fg_before {
            eprintln!("the foreground window changed during the test (the user switched apps)");
        }
        drop(pins);
        drop(normal);
        drop(frames);
        drop(normal_frame);
        assert_eq!(ledger.live(), (0, 0));
    }

    /// The picture really reaches the window, and stays there after Noeul's temporary DIB was
    /// freed: DWM renders the layered window's retained surface into a bitmap of ours
    /// (`PrintWindow` with `PW_RENDERFULLCONTENT`), at 100 % and zoomed.
    #[test]
    fn the_window_keeps_showing_the_pictures_pixels_after_the_upload() {
        #[link(name = "user32")]
        unsafe extern "system" {
            fn PrintWindow(hwnd: HWND, hdc: HDC, flags: u32) -> i32;
        }
        const PW_RENDERFULLCONTENT: u32 = 2;
        let ledger = Ledger::new();
        let (w, h) = (8u32, 6u32);
        let mut px = Vec::new();
        for y in 0..h {
            for x in 0..w {
                px.extend_from_slice(&[x as u8 * 30, y as u8 * 40, 200, 255]);
            }
        }
        let frame = Frame::new(&ledger, Rect::new(FAR, FAR, w, h), px).unwrap();
        let mut pin = create(
            PinId(301),
            &frame,
            &spec(frame.rect()),
            Style::OFF_SCREEN_TEST,
        )
        .unwrap();
        let shot = |pin: &WinPin, w: u32, h: u32| -> Option<Vec<u8>> {
            let mut s = Surface::new(w, h).unwrap();
            // SAFETY: our window renders into our own DIB section of its size.
            let ok = unsafe { PrintWindow(pin.hwnd(), s.dc, PW_RENDERFULLCONTENT) } != 0;
            ok.then(|| s.pixels().to_vec())
        };
        let Some(got) = shot(&pin, w, h) else {
            eprintln!("SKIP: PrintWindow could not render the window on this system");
            return;
        };
        let rgb = |b: &[u8], i: usize| [b[i * 4], b[i * 4 + 1], b[i * 4 + 2]];
        for i in 0..(w * h) as usize {
            assert_eq!(rgb(&got, i), rgb(frame.pixels(), i), "pixel {i} at 100 %");
        }
        // 200 %: every picture pixel is a 2×2 block of the window.
        pin.present(&frame, Rect::new(FAR, FAR, w * 2, h * 2))
            .unwrap();
        let got = shot(&pin, w * 2, h * 2).expect("rendered");
        for (x, y) in [(0u32, 0u32), (3, 2), (7, 5)] {
            let src = (y * w + x) as usize;
            for (dx, dy) in [(0, 0), (1, 1)] {
                let dst = ((y * 2 + dy) * w * 2 + x * 2 + dx) as usize;
                assert_eq!(
                    rgb(&got, dst),
                    rgb(frame.pixels(), src),
                    "({x},{y}) at 200 %"
                );
            }
        }
        drop(pin);
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
        assert_eq!(point_of(0xFFFF_FFFFu32 as isize), (-1, -1));
        assert_eq!(point_of(((-5i16 as u16 as isize) << 16) | 7), (7, -5));
    }
}
