//! The capture overlay's Win32 window (NOEUL-FR-010, NOEUL-FR-090, ADR 0026).
//!
//! - **One window over the virtual desktop**, created when a capture starts and destroyed when
//!   it ends: a borderless popup tool window (no taskbar or Alt+Tab entry), always on top, at the
//!   frozen picture's rectangle in device pixels (the process is per-monitor DPI aware v2, so a
//!   window spanning monitors of different scales is never stretched; `WM_DPICHANGED` is
//!   ignored). Nothing of it exists while Noeul is idle (NOEUL-NFR-002).
//! - **Drawing:** a back buffer (a 32-bit DIB section of the picture's size) is composed by
//!   [`crate::overlay::compose`] for the parts that changed, the labels' text is drawn into it with
//!   GDI (clipped to those parts), and `WM_PAINT` copies the invalid region to the screen with one
//!   `BitBlt`. No timer, no animation: the overlay paints only when something changed.
//! - **Input stays Noeul's own:** pointer and key messages of this window become
//!   [`OverlayEvent`]s for the instance core, queued and handed over through the waker like the
//!   pin windows'. The pointer is captured during a press (`SetCapture`) and nothing is ever
//!   synthesized (NOEUL-SEC-012). The input method is switched off for the window so letter
//!   shortcuts work in Hangul mode (NOEUL-NFR-014). A press counts only from raw button messages
//!   (the class has no `CS_DBLCLKS`; the double-click gesture of Step 51 counts presses itself).
//! - **Focus:** a real capture brings the window to the foreground (the process that received
//!   the hotkey may); when another program's window becomes active the overlay is cancelled
//!   (NOEUL-FR-013). The test placement ([`Placement::OFF_SCREEN_TEST`]) never activates, never
//!   stays on top and never touches the cursor.
//! - **Keys** (Step 52): every key message carries its logical chord and, from the scan code in
//!   `lParam`, its physical chord (`code:KeyW`…), so `W A S D` keep their position on every
//!   layout. A pointer key moves the real cursor with `SetCursorPos` only while this window is
//!   the foreground window and never under the test placement (NOEUL-SEC-012).
//! - **Annotation** (Step 56): typed characters (`WM_CHAR`, surrogate pairs joined), input-method
//!   messages (`WM_IME_STARTCOMPOSITION`, `WM_IME_COMPOSITION` with the composition and result
//!   strings read from the window's input context, `WM_IME_ENDCOMPOSITION`) and the wheel become
//!   events; the composition window of the IME is hidden because the label shows the composition
//!   inline. The input method is associated with the window only while a label is edited
//!   ([`OverlayWindow::set_ime`]), with its candidate window at the caret; it is never switched
//!   for the user. Labels are typeset with GDI and font fallback (`win::typeset`). The pin editor
//!   uses a **layered** window (`UpdateLayeredWindow` with per-pixel alpha), so the canvas around
//!   the pin is transparent and passes clicks to what is behind.

use crate::annotation::{Glyphs, Ime, Typesetter};
use crate::app::{OverlayEvent, OverlayWindow};
use crate::frame::{Frame, Rect};
use crate::hotkeys::{chord_of, physical_chord};
use crate::overlay::{self, Button, Cursor, Font, Input, Measure, Mods, Scene, color};
use crate::win::pinwnd::vk_name;
use crate::win::typeset::GdiType;
use crate::win::wide;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::OnceLock;
use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION, GetDC, ReleaseDC,
};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BeginPaint, BitBlt, CLEARTYPE_QUALITY,
    CLIP_DEFAULT_PRECIS, CreateCompatibleDC, CreateDIBSection, CreateFontW, CreateRectRgn,
    DEFAULT_CHARSET, DIB_RGB_COLORS, DeleteDC, DeleteObject, EndPaint, FF_MODERN, FF_SWISS,
    FIXED_PITCH, FW_NORMAL, GdiFlush, GetTextExtentPoint32W, GetTextFaceW, HBITMAP, HDC, HFONT,
    InvalidateRect, OUT_DEFAULT_PRECIS, PAINTSTRUCT, SRCCOPY, SelectClipRgn, SelectObject,
    SetBkMode, SetTextColor, TRANSPARENT, TextOutW, UpdateWindow, VARIABLE_PITCH,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::Ime::{
    CANDIDATEFORM, CFS_CANDIDATEPOS, CFS_POINT, COMPOSITIONFORM, GCS_COMPSTR, GCS_RESULTSTR,
    IACE_DEFAULT, ISC_SHOWUICOMPOSITIONWINDOW, ImmAssociateContextEx, ImmGetCompositionStringW,
    ImmGetContext, ImmReleaseContext, ImmSetCandidateWindow, ImmSetCompositionWindow,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetDoubleClickTime, GetKeyState, ReleaseCapture, SetCapture, SetFocus, VK_APPS, VK_CONTROL,
    VK_F10, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA, GetMessageTime,
    GetSystemMetrics, GetWindowLongPtrW, GetWindowThreadProcessId, HCURSOR, HTCLIENT, IDC_ARROW,
    IDC_CROSS, IDC_SIZEALL, IDC_SIZENESW, IDC_SIZENS, IDC_SIZENWSE, IDC_SIZEWE, IsWindow,
    IsWindowVisible, LoadCursorW, MA_NOACTIVATE, RegisterClassExW, SM_CXDOUBLECLK, SM_CXDRAG,
    SM_CYDOUBLECLK, SM_CYDRAG, SW_SHOW, SW_SHOWNOACTIVATE, SetCursor, SetForegroundWindow,
    SetWindowLongPtrW, ShowWindow, WA_INACTIVE, WM_ACTIVATE, WM_CAPTURECHANGED, WM_CLOSE,
    WM_DESTROY, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_ERASEBKGND, WM_KEYDOWN, WM_KEYUP,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_PAINT,
    WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETCURSOR, WM_SYSKEYDOWN, WM_SYSKEYUP, WNDCLASSEXW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, SetCursorPos};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    ULW_ALPHA, UpdateLayeredWindow, WM_CHAR, WM_IME_CHAR, WM_IME_COMPOSITION,
    WM_IME_ENDCOMPOSITION, WM_IME_SETCONTEXT, WM_IME_STARTCOMPOSITION, WM_MOUSEWHEEL,
    WS_EX_LAYERED,
};

const CLASS: &str = "NoeulOverlay";

thread_local! {
    static EVENTS: RefCell<VecDeque<OverlayEvent>> = const { RefCell::new(VecDeque::new()) };
    static WAKER: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
    /// The open overlay's origin (its client (0,0) in virtual-desktop pixels), whether it may be
    /// activated, its cursor, the last pointer position, and whether we release the capture.
    static ORIGIN: Cell<(i32, i32)> = const { Cell::new((0, 0)) };
    static ACTIVATES: Cell<bool> = const { Cell::new(false) };
    static CURSOR: Cell<usize> = const { Cell::new(0) };
    static LAST: Cell<(i32, i32)> = const { Cell::new((0, 0)) };
    static RELEASING: Cell<bool> = const { Cell::new(false) };
    /// The open overlay window (the owner of its context menu and of the save dialog).
    static CURRENT: Cell<usize> = const { Cell::new(0) };
    /// The first half of a character outside the BMP (`WM_CHAR` sends UTF-16 units).
    static HIGH: Cell<u16> = const { Cell::new(0) };
}

#[cfg(test)]
thread_local! {
    /// What a test's stand-in input context holds: `(composition, result)` (tests never use the
    /// user's input method).
    pub(crate) static IME_TEST: RefCell<Option<(String, String)>> = const { RefCell::new(None) };
}

/// The composition (`GCS_COMPSTR`) or result (`GCS_RESULTSTR`) string of the window's input
/// context.
fn comp_string(hwnd: HWND, which: u32) -> Option<String> {
    #[cfg(test)]
    if let Some((comp, result)) = IME_TEST.with(|t| t.borrow().clone()) {
        return Some(if which == GCS_RESULTSTR { result } else { comp });
    }
    // SAFETY: the input context of our own window, released below; the buffer has the byte
    // length the first call reported.
    unsafe {
        let himc = ImmGetContext(hwnd);
        if himc.is_null() {
            return None;
        }
        let bytes = ImmGetCompositionStringW(himc, which, std::ptr::null_mut(), 0);
        let out = if bytes > 0 {
            let mut buf = vec![0u16; usize::try_from(bytes).unwrap_or(0) / 2];
            let got = ImmGetCompositionStringW(
                himc,
                which,
                buf.as_mut_ptr().cast(),
                u32::try_from(bytes).unwrap_or(0),
            );
            buf.truncate(usize::try_from(got.max(0)).unwrap_or(0) / 2);
            Some(String::from_utf16_lossy(&buf))
        } else {
            Some(String::new())
        };
        ImmReleaseContext(hwnd, himc);
        out
    }
}

/// A UTF-16 unit of a typed character: whole characters are pushed, surrogate pairs joined.
fn char_unit(unit: u16, repeat: bool) {
    match unit {
        0xD800..=0xDBFF => HIGH.set(unit),
        0xDC00..=0xDFFF => {
            let high = HIGH.replace(0);
            if high != 0
                && let Some(Ok(c)) = char::decode_utf16([high, unit]).next()
            {
                push(OverlayEvent::Char(c, repeat));
            }
        }
        _ => {
            HIGH.set(0);
            if let Some(c) = char::from_u32(u32::from(unit)) {
                push(OverlayEvent::Char(c, repeat));
            }
        }
    }
}

/// The open overlay window, if any (same thread).
pub fn current() -> Option<HWND> {
    let h = CURRENT.get() as HWND;
    // SAFETY: a liveness query on a handle we created.
    (!h.is_null() && unsafe { IsWindow(h) } != 0).then_some(h)
}

/// Sets what runs after the overlay queued an event (the instance's pump). Same thread.
pub fn set_waker(f: Option<Box<dyn Fn()>>) {
    WAKER.with(|w| *w.borrow_mut() = f);
}

/// Takes the queued overlay events (in order).
pub fn take_events() -> Vec<OverlayEvent> {
    EVENTS.with(|e| e.borrow_mut().drain(..).collect())
}

fn push(e: OverlayEvent) {
    EVENTS.with(|q| q.borrow_mut().push_back(e));
    WAKER.with(|w| {
        if let Ok(w) = w.try_borrow()
            && let Some(f) = w.as_ref()
        {
            f();
        }
    });
}

fn down(vk: u16) -> bool {
    // SAFETY: plain key-state query for this thread's input.
    unsafe { GetKeyState(i32::from(vk)) < 0 }
}

fn mods() -> Mods {
    Mods {
        ctrl: down(VK_CONTROL),
        shift: down(VK_SHIFT),
        alt: down(VK_MENU),
        win: down(VK_LWIN) || down(VK_RWIN),
    }
}

/// Client coordinates of a mouse message, as virtual-desktop device pixels.
fn point(lp: LPARAM) -> (i32, i32) {
    let x = i32::from((lp & 0xFFFF) as u16 as i16);
    let y = i32::from(((lp >> 16) & 0xFFFF) as u16 as i16);
    let (ox, oy) = ORIGIN.get();
    (ox + x, oy + y)
}

fn time() -> u32 {
    // SAFETY: the time of the message being processed.
    unsafe { GetMessageTime() as u32 }
}

/// Whether `hwnd` belongs to this process (a pin, the control window).
fn ours(hwnd: HWND) -> bool {
    if hwnd.is_null() {
        return false;
    }
    let mut pid = 0u32;
    // SAFETY: a window handle from a message; `pid` is writable.
    unsafe { GetWindowThreadProcessId(hwnd, &raw mut pid) };
    pid == std::process::id()
}

fn input(i: Input) {
    push(OverlayEvent::Input(i));
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            // SAFETY: PAINTSTRUCT is plain data; BeginPaint fills it.
            let mut ps: PAINTSTRUCT = unsafe { std::mem::zeroed() };
            // SAFETY: our window, in its WM_PAINT.
            let hdc = unsafe { BeginPaint(hwnd, &raw mut ps) };
            // SAFETY: GWLP_USERDATA holds our back buffer's memory DC (0 while being destroyed).
            let mem = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as HDC;
            if !hdc.is_null() && !mem.is_null() {
                let r = ps.rcPaint;
                // SAFETY: both DCs are valid; client and back buffer share their origin.
                unsafe {
                    BitBlt(
                        hdc,
                        r.left,
                        r.top,
                        r.right - r.left,
                        r.bottom - r.top,
                        mem,
                        r.left,
                        r.top,
                        SRCCOPY,
                    )
                };
            }
            // SAFETY: pairs the BeginPaint above.
            unsafe { EndPaint(hwnd, &raw const ps) };
            return 0;
        }
        WM_ERASEBKGND => return 1,
        WM_SETCURSOR => {
            if (lp & 0xFFFF) as u32 == HTCLIENT && CURSOR.get() != 0 {
                // SAFETY: a shared system cursor loaded by `set_cursor`.
                unsafe { SetCursor(CURSOR.get() as HCURSOR) };
                return 1;
            }
        }
        WM_MOUSEACTIVATE if !ACTIVATES.get() => return MA_NOACTIVATE as LRESULT,
        WM_MOUSEMOVE => {
            let (x, y) = point(lp);
            LAST.set((x, y));
            input(Input::Move { x, y, mods: mods() });
            return 0;
        }
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
            let (x, y) = point(lp);
            LAST.set((x, y));
            let button = match msg {
                WM_LBUTTONDOWN => Button::Left,
                WM_RBUTTONDOWN => Button::Right,
                _ => Button::Middle,
            };
            if button == Button::Left && ACTIVATES.get() {
                // SAFETY: our (foreground) window takes the pointer for the duration of the
                // press. The test placement never captures the pointer.
                unsafe { SetCapture(hwnd) };
            }
            input(Input::Press {
                button,
                x,
                y,
                mods: mods(),
                time_ms: time(),
            });
            return 0;
        }
        WM_LBUTTONUP => {
            let (x, y) = point(lp);
            input(Input::Release {
                button: Button::Left,
                x,
                y,
            });
            if ACTIVATES.get() {
                RELEASING.set(true);
                // SAFETY: releases the capture taken on the press (harmless if none).
                unsafe { ReleaseCapture() };
                RELEASING.set(false);
            }
            return 0;
        }
        // No WM_CONTEXTMENU: right-click is the overlay's own (clear or cancel).
        WM_RBUTTONUP => return 0,
        WM_CAPTURECHANGED => {
            if !RELEASING.get() && lp as HWND != hwnd {
                input(Input::CaptureLost);
            }
            return 0;
        }
        WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP => {
            let vk = u32::try_from(wp).unwrap_or(0);
            if vk == u32::from(VK_SHIFT) {
                // Shift changes the shape of a drag (a square) without a pointer move.
                let (x, y) = LAST.get();
                input(Input::Move { x, y, mods: mods() });
                return 0;
            }
            if msg == WM_KEYUP || msg == WM_SYSKEYUP {
                return 0;
            }
            let repeat = (lp >> 30) & 1 == 1;
            // The menu key and Shift+F10 open the selection's context menu (reserved keys).
            if vk == u32::from(VK_APPS)
                || (vk == u32::from(VK_F10)
                    && down(VK_SHIFT)
                    && !down(VK_CONTROL)
                    && !down(VK_MENU))
            {
                if !repeat {
                    input(Input::MenuKey);
                }
                return 0;
            }
            let name = vk_name(vk);
            if msg == WM_SYSKEYDOWN && name.as_deref() == Some("F4") {
                // Alt+F4 (reserved `window.close`) leaves the overlay.
                input(Input::Leave);
                return 0;
            }
            let (ctrl, alt, shift) = (down(VK_CONTROL), down(VK_MENU), down(VK_SHIFT));
            // The key's position: its scan code (bits 16–23) and the extended flag (bit 24).
            let code = physical_chord(
                ((lp >> 16) & 0xFF) as u32,
                (lp >> 24) & 1 == 1,
                ctrl,
                alt,
                shift,
            );
            let logical = name.and_then(|n| chord_of(&n, ctrl, alt, shift));
            match (logical, code) {
                (Some(chord), code) => push(OverlayEvent::Key(chord, code, repeat)),
                // A key without a logical name (punctuation on some layouts) still has its
                // position.
                (None, Some(code)) => push(OverlayEvent::Key(code, None, repeat)),
                (None, None) => {}
            }
            return 0;
        }
        WM_ACTIVATE => {
            if (wp & 0xFFFF) as u32 == WA_INACTIVE && !ours(lp as HWND) {
                input(Input::FocusLost);
            }
            return 0;
        }
        WM_CHAR | WM_IME_CHAR => {
            char_unit((wp & 0xFFFF) as u16, (lp >> 30) & 1 == 1);
            return 0;
        }
        WM_MOUSEWHEEL => {
            let delta = i32::from(((wp >> 16) & 0xFFFF) as u16 as i16);
            push(OverlayEvent::Wheel(delta));
            return 0;
        }
        WM_IME_SETCONTEXT => {
            // The label shows the composition itself: no composition window of the IME.
            let lp = lp & !(ISC_SHOWUICOMPOSITIONWINDOW as LPARAM);
            // SAFETY: default processing of a message of our own window.
            return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
        }
        WM_IME_STARTCOMPOSITION => {
            push(OverlayEvent::Ime(Ime::Start));
            return 0;
        }
        WM_IME_COMPOSITION => {
            let flags = lp as u32;
            if flags & GCS_RESULTSTR != 0
                && let Some(text) = comp_string(hwnd, GCS_RESULTSTR)
            {
                push(OverlayEvent::Ime(Ime::Commit(text)));
            }
            if flags & GCS_COMPSTR != 0 {
                let text = comp_string(hwnd, GCS_COMPSTR).unwrap_or_default();
                push(OverlayEvent::Ime(Ime::Compose(text)));
            }
            return 0;
        }
        WM_IME_ENDCOMPOSITION => {
            push(OverlayEvent::Ime(Ime::End));
            return 0;
        }
        // The frozen picture no longer matches the monitors: end the capture.
        WM_DISPLAYCHANGE | WM_CLOSE => {
            input(Input::Leave);
            return 0;
        }
        // Keep the device-pixel rectangle on every monitor (per-monitor DPI aware v2).
        WM_DPICHANGED | WM_DESTROY => return 0,
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
            lpfnWndProc: Some(wndproc),
            // SAFETY: the module handle of this executable.
            hInstance: unsafe { GetModuleHandleW(std::ptr::null()) },
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
        Ok(Box::leak(name.into_boxed_slice()).as_ptr() as usize)
    });
    name.clone().map(|p| p as *const u16)
}

/// How the overlay window is placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// Above every window (`WS_EX_TOPMOST`).
    pub topmost: bool,
    /// Brought to the foreground with keyboard focus and the overlay's cursor.
    pub activate: bool,
}

impl Placement {
    /// A real capture.
    pub const INTERACTIVE: Placement = Placement {
        topmost: true,
        activate: true,
    };
    /// Tests and the debug-build test hook: the window lies off-screen (its picture is), is never
    /// activated, never on top, and never changes the cursor; the user sees and feels nothing.
    pub const OFF_SCREEN_TEST: Placement = Placement {
        topmost: false,
        activate: false,
    };
}

/// The overlay window with its back buffer.
pub struct WinOverlay {
    hwnd: HWND,
    picture: Rect,
    placement: Placement,
    /// A layered window (the pin editor, Step 56).
    layered: bool,
    typeset: GdiType,
    mem: HDC,
    bitmap: HBITMAP,
    old: *mut c_void,
    bits: *mut u8,
    len: usize,
    fonts: RefCell<Vec<(Font, u32, HFONT)>>,
}

impl std::fmt::Debug for WinOverlay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WinOverlay")
            .field("hwnd", &self.hwnd)
            .field("picture", &self.picture)
            .finish_non_exhaustive()
    }
}

/// `COLORREF` (0x00BBGGRR) of an RGB triple.
fn rgb(c: [u8; 3]) -> COLORREF {
    u32::from(c[0]) | (u32::from(c[1]) << 8) | (u32::from(c[2]) << 16)
}

impl WinOverlay {
    /// Creates the (hidden) window over `picture` with a back buffer of its size; `layered`
    /// for the pin editor (per-pixel alpha, Step 56).
    pub fn create(
        picture: Rect,
        placement: Placement,
        layered: bool,
    ) -> Result<WinOverlay, String> {
        let class = class()?;
        let (w, h) = (
            i32::try_from(picture.width).map_err(|_| "too wide")?,
            i32::try_from(picture.height).map_err(|_| "too high")?,
        );
        let mut ex = WS_EX_TOOLWINDOW;
        if placement.topmost {
            ex |= WS_EX_TOPMOST;
        }
        if !placement.activate {
            ex |= WS_EX_NOACTIVATE;
        }
        if layered {
            ex |= WS_EX_LAYERED;
        }
        let title = wide(if layered {
            "Noeul pin annotation"
        } else {
            "Noeul capture"
        });
        // SAFETY: registered class; plain values; no parent, menu or creation data.
        let hwnd = unsafe {
            CreateWindowExW(
                ex,
                class,
                title.as_ptr(),
                WS_POPUP,
                picture.x,
                picture.y,
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
        let mut o = WinOverlay {
            hwnd,
            picture,
            placement,
            layered,
            typeset: GdiType::new(),
            mem: std::ptr::null_mut(),
            bitmap: std::ptr::null_mut(),
            old: std::ptr::null_mut(),
            bits: std::ptr::null_mut(),
            len: picture.bgra_bytes() as usize,
            fonts: RefCell::new(Vec::new()),
        };
        // SAFETY: a memory DC compatible with the screen; deleted in `destroy`.
        o.mem = unsafe { CreateCompatibleDC(std::ptr::null_mut()) };
        if o.mem.is_null() {
            o.destroy();
            return Err("CreateCompatibleDC failed".into());
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: u32::try_from(size_of::<BITMAPINFOHEADER>()).expect("small"),
                biWidth: w,
                biHeight: -h, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        // SAFETY: `info` describes a top-down 32-bit DIB of the picture's size.
        o.bitmap = unsafe {
            CreateDIBSection(
                o.mem,
                &raw const info,
                DIB_RGB_COLORS,
                &raw mut bits,
                std::ptr::null_mut(),
                0,
            )
        };
        if o.bitmap.is_null() || bits.is_null() {
            o.destroy();
            return Err("CreateDIBSection failed (out of memory?)".into());
        }
        o.bits = bits.cast::<u8>();
        // SAFETY: selects our bitmap into our DC (restored in `destroy`); then stores the DC for
        // WM_PAINT and switches the input method off for the window.
        unsafe {
            o.old = SelectObject(o.mem, o.bitmap);
            SetBkMode(o.mem, TRANSPARENT as i32);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, o.mem as isize);
            ImmAssociateContextEx(hwnd, std::ptr::null_mut(), 0);
        }
        ORIGIN.set((picture.x, picture.y));
        ACTIVATES.set(placement.activate);
        LAST.set((picture.x, picture.y));
        CURRENT.set(hwnd as usize);
        Ok(o)
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// The back buffer (BGRA, top-down, the picture's size).
    pub fn buffer(&self) -> &[u8] {
        if self.bits.is_null() {
            return &[];
        }
        // SAFETY: the DIB section holds `len` bytes at `bits` while it is alive; GDI finished
        // writing (flushed after every present).
        unsafe { std::slice::from_raw_parts(self.bits, self.len) }
    }

    fn buffer_mut(&mut self) -> &mut [u8] {
        if self.bits.is_null() {
            return &mut [];
        }
        // SAFETY: as in `buffer`; `&mut self` makes this the only access.
        unsafe { std::slice::from_raw_parts_mut(self.bits, self.len) }
    }

    /// A GDI font for `font` at `dpi` (cached for the window's life).
    fn font(&self, font: Font, dpi: u32) -> HFONT {
        if let Some(f) = self
            .fonts
            .borrow()
            .iter()
            .find(|(k, d, _)| *k == font && *d == dpi)
        {
            return f.2;
        }
        let (size, faces, pitch): (u32, &[&str], u32) = match font {
            Font::Mono => (
                11,
                &["Cascadia Mono", "Consolas"],
                u32::from(FIXED_PITCH | FF_MODERN),
            ),
            Font::Ui => (12, &["Segoe UI"], u32::from(VARIABLE_PITCH | FF_SWISS)),
        };
        let height = -i32::try_from(crate::capture::scale_px(size, dpi)).unwrap_or(11);
        let mut chosen: HFONT = std::ptr::null_mut();
        for (i, face) in faces.iter().enumerate() {
            let name = wide(face);
            // SAFETY: plain values and a NUL-terminated face name.
            let f = unsafe {
                CreateFontW(
                    height,
                    0,
                    0,
                    0,
                    FW_NORMAL as i32,
                    0,
                    0,
                    0,
                    u32::from(DEFAULT_CHARSET),
                    u32::from(OUT_DEFAULT_PRECIS),
                    u32::from(CLIP_DEFAULT_PRECIS),
                    u32::from(CLEARTYPE_QUALITY),
                    pitch,
                    name.as_ptr(),
                )
            };
            if f.is_null() {
                continue;
            }
            // The first face that is really installed (GDI substitutes silently); the last
            // candidate is taken as it comes.
            if i + 1 == faces.len() || self.face_is(f, face) {
                chosen = f;
                break;
            }
            // SAFETY: a font we created and do not keep.
            unsafe { DeleteObject(f) };
        }
        self.fonts.borrow_mut().push((font, dpi, chosen));
        chosen
    }

    /// Whether GDI really gave `f` the face `face`.
    fn face_is(&self, f: HFONT, face: &str) -> bool {
        let mut got = [0u16; 64];
        // SAFETY: selects the font briefly into our DC to read its face name.
        let n = unsafe {
            let prev = SelectObject(self.mem, f);
            let n = GetTextFaceW(self.mem, 64, got.as_mut_ptr());
            SelectObject(self.mem, prev);
            n
        };
        let n = usize::try_from(n).unwrap_or(0).min(64);
        String::from_utf16_lossy(&got[..n])
            .trim_end_matches('\0')
            .eq_ignore_ascii_case(face)
    }

    /// Clips the back buffer's DC to `area` (client coordinates), once per area.
    fn clip(&self, area: Rect, clipped: &mut bool) {
        if *clipped {
            return;
        }
        let (ox, oy) = (self.picture.x, self.picture.y);
        // SAFETY: a region in client coordinates, selected (copied) into our DC and deleted
        // right after.
        unsafe {
            let rgn = CreateRectRgn(
                area.x - ox,
                area.y - oy,
                area.x - ox + i32::try_from(area.width).unwrap_or(0),
                area.y - oy + i32::try_from(area.height).unwrap_or(0),
            );
            SelectClipRgn(self.mem, rgn);
            DeleteObject(rgn);
        }
        *clipped = true;
    }

    /// Draws the text of the labels, the magnifier's readout and the toolbar buttons that
    /// intersect `area`, clipped to it.
    fn draw_text(&self, scene: &Scene, area: Rect) {
        let (ox, oy) = (self.picture.x, self.picture.y);
        let labels = scene
            .chip
            .iter()
            .chain(scene.help.iter())
            .chain(scene.magnifier.iter().map(|m| &m.text));
        let mut clipped = false;
        if let Some(bar) = &scene.toolbar
            && bar.rect.intersects(&area)
        {
            self.clip(area, &mut clipped);
            let f = self.font(Font::Ui, bar.dpi);
            for b in bar.buttons.iter().filter(|b| !b.label.is_empty()) {
                let text: Vec<u16> = b.label.encode_utf16().collect();
                let (tw, th) = self.text(Font::Ui, bar.dpi, b.label);
                let x = b.rect.x
                    + (i32::try_from(b.rect.width).unwrap_or(0) - i32::try_from(tw).unwrap_or(0))
                        / 2;
                let y = b.rect.y
                    + (i32::try_from(b.rect.height).unwrap_or(0) - i32::try_from(th).unwrap_or(0))
                        / 2;
                let c = if b.primary {
                    color::TEXT_ON_ACCENT
                } else {
                    color::TEXT_PRIMARY
                };
                // SAFETY: our DC and font; NUL-free UTF-16 text with its length.
                unsafe {
                    let prev = SelectObject(self.mem, f);
                    SetTextColor(self.mem, rgb(c));
                    TextOutW(
                        self.mem,
                        x - ox,
                        y - oy,
                        text.as_ptr(),
                        i32::try_from(text.len()).unwrap_or(0),
                    );
                    SelectObject(self.mem, prev);
                }
            }
        }
        for label in labels {
            if !label.rect.intersects(&area) {
                continue;
            }
            self.clip(area, &mut clipped);
            let f = self.font(label.font, label.dpi);
            // SAFETY: our DC, font and NUL-free UTF-16 text with its length.
            unsafe {
                let prev = SelectObject(self.mem, f);
                for (i, line) in label.lines.iter().enumerate() {
                    let c = if i == 0 {
                        color::TEXT_PRIMARY
                    } else {
                        color::TEXT_SECONDARY
                    };
                    SetTextColor(self.mem, rgb(c));
                    let text: Vec<u16> = line.encode_utf16().collect();
                    let (x, y) = label.line_origin(i);
                    TextOutW(
                        self.mem,
                        x - ox,
                        y - oy,
                        text.as_ptr(),
                        i32::try_from(text.len()).unwrap_or(0),
                    );
                }
                SelectObject(self.mem, prev);
            }
        }
        if clipped {
            // SAFETY: removes the clip region from our DC.
            unsafe { SelectClipRgn(self.mem, std::ptr::null_mut()) };
        }
    }

    fn alive(&self) -> bool {
        // SAFETY: a liveness query on a handle we created.
        !self.hwnd.is_null() && unsafe { IsWindow(self.hwnd) } != 0
    }
}

impl Measure for WinOverlay {
    fn text(&self, font: Font, dpi: u32, text: &str) -> (u32, u32) {
        if self.mem.is_null() {
            return overlay::Approx.text(font, dpi, text);
        }
        let f = self.font(font, dpi);
        let w: Vec<u16> = text.encode_utf16().collect();
        let mut size = SIZE::default();
        // SAFETY: our DC and font; `w` and its length; `size` is writable.
        let ok = unsafe {
            let prev = SelectObject(self.mem, f);
            let ok = GetTextExtentPoint32W(
                self.mem,
                w.as_ptr(),
                i32::try_from(w.len()).unwrap_or(0),
                &raw mut size,
            );
            SelectObject(self.mem, prev);
            ok
        };
        if ok == 0 {
            return overlay::Approx.text(font, dpi, text);
        }
        (
            u32::try_from(size.cx).unwrap_or(0),
            u32::try_from(size.cy).unwrap_or(0),
        )
    }
}

impl Typesetter for WinOverlay {
    fn typeset(&self, text: &str, size: u32) -> Glyphs {
        self.typeset.typeset(text, size)
    }
}

impl WinOverlay {
    /// GDI leaves the alpha of the pixels it draws text on at 0: a layered window makes the
    /// label boxes and the toolbar opaque again.
    fn opaque_labels(&mut self, scene: &Scene, dirty: &[Rect]) {
        let pic = self.picture;
        let mut boxes: Vec<Rect> = scene
            .chip
            .iter()
            .chain(scene.help.iter())
            .map(|l| l.rect)
            .collect();
        boxes.extend(scene.toolbar.iter().map(|t| t.rect));
        boxes.extend(scene.magnifier.iter().map(|m| m.rect));
        let stride = pic.width as usize * 4;
        let buf = self.buffer_mut();
        for b in boxes {
            for d in dirty {
                let Some(r) =
                    overlay::intersection(b, *d).and_then(|r| overlay::intersection(r, pic))
                else {
                    continue;
                };
                for y in r.y..r.y + r.height as i32 {
                    let row = (i64::from(y) - i64::from(pic.y)) as usize * stride;
                    for x in r.x..r.x + r.width as i32 {
                        let i = row + (i64::from(x) - i64::from(pic.x)) as usize * 4 + 3;
                        buf[i] = 0xFF;
                    }
                }
            }
        }
    }

    /// Hands the whole back buffer to the layered window (it keeps its own copy).
    fn update_layered(&self) {
        let size = SIZE {
            cx: i32::try_from(self.picture.width).unwrap_or(0),
            cy: i32::try_from(self.picture.height).unwrap_or(0),
        };
        let at = POINT {
            x: self.picture.x,
            y: self.picture.y,
        };
        let src = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        // SAFETY: our layered window and back buffer DC (premultiplied 32-bit DIB); the screen DC
        // is released at once.
        unsafe {
            let screen = GetDC(std::ptr::null_mut());
            UpdateLayeredWindow(
                self.hwnd,
                screen,
                &raw const at,
                &raw const size,
                self.mem,
                &raw const src,
                0,
                &raw const blend,
                ULW_ALPHA,
            );
            ReleaseDC(std::ptr::null_mut(), screen);
        }
    }
}

impl OverlayWindow for WinOverlay {
    fn present(&mut self, frozen: &Frame, layer: Option<&Frame>, scene: &Scene, dirty: &[Rect]) {
        if !self.alive() || frozen.rect() != self.picture {
            return;
        }
        // SAFETY: finishes pending GDI work before the DIB's memory is written directly.
        unsafe { GdiFlush() };
        for r in dirty {
            overlay::compose(frozen, layer, scene, *r, self.buffer_mut());
        }
        for r in dirty {
            self.draw_text(scene, *r);
        }
        // SAFETY: as above, before WM_PAINT reads the buffer.
        unsafe { GdiFlush() };
        if self.layered {
            self.opaque_labels(scene, dirty);
            self.update_layered();
            return;
        }
        let (ox, oy) = (self.picture.x, self.picture.y);
        for r in dirty {
            let rc = RECT {
                left: r.x - ox,
                top: r.y - oy,
                right: r.x - ox + i32::try_from(r.width).unwrap_or(0),
                bottom: r.y - oy + i32::try_from(r.height).unwrap_or(0),
            };
            // SAFETY: our window; the rectangle is in client coordinates; no erase.
            unsafe { InvalidateRect(self.hwnd, &raw const rc, 0) };
        }
        // SAFETY: paints the invalid parts now (WM_PAINT copies them from the back buffer).
        unsafe { UpdateWindow(self.hwnd) };
    }

    fn show(&mut self, activate: bool) -> bool {
        if !self.alive() {
            return false;
        }
        let activate = activate && self.placement.activate;
        // SAFETY: shows our window; a real capture brings it to the foreground (allowed for the
        // process that received the hotkey or the user's click) and paints it at once.
        unsafe {
            ShowWindow(
                self.hwnd,
                if activate { SW_SHOW } else { SW_SHOWNOACTIVATE },
            );
            if activate {
                SetForegroundWindow(self.hwnd);
                SetFocus(self.hwnd);
                if CURSOR.get() != 0 {
                    SetCursor(CURSOR.get() as HCURSOR);
                }
            }
            UpdateWindow(self.hwnd);
            IsWindowVisible(self.hwnd) != 0
        }
    }

    fn set_cursor(&mut self, cursor: Cursor) {
        let id = match cursor {
            Cursor::Cross => IDC_CROSS,
            Cursor::Arrow => IDC_ARROW,
            Cursor::Move => IDC_SIZEALL,
            Cursor::SizeWe => IDC_SIZEWE,
            Cursor::SizeNs => IDC_SIZENS,
            Cursor::SizeNwse => IDC_SIZENWSE,
            Cursor::SizeNesw => IDC_SIZENESW,
        };
        // SAFETY: a shared system cursor (never destroyed).
        let h = unsafe { LoadCursorW(std::ptr::null_mut(), id) };
        let changed = CURSOR.get() != h as usize;
        CURSOR.set(h as usize);
        if changed && self.placement.activate && self.alive() {
            // SAFETY: as above; only a real, active overlay changes the visible cursor.
            unsafe { SetCursor(h) };
        }
    }

    fn buffer_bytes(&self) -> u64 {
        if self.bits.is_null() {
            0
        } else {
            self.len as u64
        }
    }

    fn set_ime(&mut self, caret: Option<Rect>) {
        if !self.alive() {
            return;
        }
        let (ox, oy) = (self.picture.x, self.picture.y);
        // SAFETY: associates (or removes) the default input context of our own window and places
        // its windows in client coordinates; the context is released at once. The user's input
        // method itself is never switched.
        unsafe {
            match caret {
                Some(c) => {
                    ImmAssociateContextEx(self.hwnd, std::ptr::null_mut(), IACE_DEFAULT);
                    let himc = ImmGetContext(self.hwnd);
                    if !himc.is_null() {
                        let pt = POINT {
                            x: c.x - ox,
                            y: c.y - oy + i32::try_from(c.height).unwrap_or(0),
                        };
                        let cf = COMPOSITIONFORM {
                            dwStyle: CFS_POINT,
                            ptCurrentPos: POINT {
                                x: c.x - ox,
                                y: c.y - oy,
                            },
                            ..std::mem::zeroed()
                        };
                        ImmSetCompositionWindow(himc, &raw const cf);
                        let cand = CANDIDATEFORM {
                            dwIndex: 0,
                            dwStyle: CFS_CANDIDATEPOS,
                            ptCurrentPos: pt,
                            ..std::mem::zeroed()
                        };
                        ImmSetCandidateWindow(himc, &raw const cand);
                        ImmReleaseContext(self.hwnd, himc);
                    }
                }
                None => {
                    ImmAssociateContextEx(self.hwnd, std::ptr::null_mut(), 0);
                }
            }
        }
    }

    fn warp_pointer(&mut self, at: (i32, i32)) {
        // SAFETY: plain queries; `SetCursorPos` only for a real overlay that is the foreground
        // window (NOEUL-SEC-012). The test placement never moves the user's cursor.
        unsafe {
            if self.placement.activate && self.alive() && GetForegroundWindow() == self.hwnd {
                SetCursorPos(at.0, at.1);
            }
        }
    }

    fn destroy(&mut self) {
        // SAFETY: every object was created by `create` (or is null) and is released once:
        // the window first (it reads the DC in WM_PAINT), then fonts, bitmap and DC.
        unsafe {
            if self.alive() {
                SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
                DestroyWindow(self.hwnd);
            }
            if CURRENT.get() == self.hwnd as usize {
                CURRENT.set(0);
            }
            self.hwnd = std::ptr::null_mut();
            for (_, _, f) in self.fonts.borrow_mut().drain(..) {
                if !f.is_null() {
                    DeleteObject(f);
                }
            }
            if !self.mem.is_null() {
                if !self.old.is_null() {
                    SelectObject(self.mem, self.old);
                }
                if !self.bitmap.is_null() {
                    DeleteObject(self.bitmap);
                }
                DeleteDC(self.mem);
            }
        }
        self.mem = std::ptr::null_mut();
        self.bitmap = std::ptr::null_mut();
        self.old = std::ptr::null_mut();
        self.bits = std::ptr::null_mut();
        CURSOR.set(0);
    }
}

impl Drop for WinOverlay {
    fn drop(&mut self) {
        self.destroy();
    }
}

/// The user's pointer settings, read now (NOEUL-NFR-012).
pub fn system_values() -> overlay::SystemValues {
    let m = |i| {
        // SAFETY: plain metric query.
        u32::try_from(unsafe { GetSystemMetrics(i) }).unwrap_or(0)
    };
    let d = overlay::SystemValues::default();
    let or = |v: u32, dflt: u32| if v == 0 { dflt } else { v };
    overlay::SystemValues {
        drag: (or(m(SM_CXDRAG), d.drag.0), or(m(SM_CYDRAG), d.drag.1)),
        // SAFETY: plain query.
        double_click_ms: or(unsafe { GetDoubleClickTime() }, d.double_click_ms),
        double_click: (
            or(m(SM_CXDOUBLECLK), d.double_click.0),
            or(m(SM_CYDOUBLECLK), d.double_click.1),
        ),
    }
}

/// Where the pointer is now (device pixels; the process is per-monitor DPI aware).
pub fn cursor_pos() -> (i32, i32) {
    let mut p = POINT::default();
    // SAFETY: `p` is writable.
    unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos(&raw mut p) };
    (p.x, p.y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{CaptureBackend, Synthetic};
    use crate::frame::Ledger;
    use crate::overlay::{Overlay, SystemValues, dim_pixel};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetForegroundWindow, GetWindowLongW, GetWindowRect, SendMessageW,
        WS_EX_APPWINDOW,
    };

    fn lp(x: i32, y: i32) -> LPARAM {
        ((y as u16 as u32) << 16 | (x as u16 as u32)) as LPARAM
    }

    /// A real overlay window over a synthetic two-monitor desktop far off-screen, never
    /// activated, not on top: messages are sent to our own window only (no real input), the
    /// state machine runs on them, and the back buffer is checked pixel by pixel. The user sees
    /// nothing.
    #[test]
    fn off_screen_overlay_window_turns_messages_into_a_selection_and_pixels() {
        crate::win::per_monitor_dpi_awareness();
        let syn = Synthetic::off_screen();
        let desk = syn.desktop();
        let picture = desk.virtual_rect.unwrap();
        let ledger = Ledger::new();
        let frozen = syn.capture(&ledger, picture).unwrap();
        let mut win = WinOverlay::create(picture, Placement::OFF_SCREEN_TEST, false).unwrap();
        let hwnd = win.hwnd();
        // SAFETY: a plain query.
        let before = unsafe { GetForegroundWindow() };
        // SAFETY: queries on our own live window.
        unsafe {
            let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
            assert_ne!(ex & WS_EX_TOOLWINDOW, 0, "no taskbar entry");
            assert_eq!(ex & WS_EX_APPWINDOW, 0);
            assert_eq!(ex & WS_EX_TOPMOST, 0, "the test window is never on top");
            assert_ne!(ex & WS_EX_NOACTIVATE, 0);
            let mut r = RECT::default();
            GetWindowRect(hwnd, &raw mut r);
            assert_eq!(
                (r.left, r.top, r.right - r.left, r.bottom - r.top),
                (picture.x, picture.y, 1120, 400),
                "device pixels across a 100 % and a 150 % monitor, not stretched"
            );
        }
        let mut model = Overlay::new(
            picture,
            desk.monitors,
            SystemValues::default(),
            (picture.x + 100, picture.y + 100),
            None,
            Some(vec![
                "Drag to select a region. Esc or right-click cancels.".into(),
            ]),
        )
        .with_magnifier(false);
        let mut scene = model.scene(&win);
        win.present(&frozen, None, &scene, &[picture]);
        win.set_cursor(model.cursor());
        assert!(win.show(true), "shown (the placement refuses activation)");
        // SAFETY: a plain query.
        let after = unsafe { GetForegroundWindow() };
        assert_eq!(after, before, "focus never moved");
        // GDI measured the help text with a real font, wider than nothing.
        let help = scene.help.clone().unwrap();
        assert!(
            help.rect.width > 100 && help.rect.height > 10,
            "{:?}",
            help.rect
        );
        let _ = take_events();
        // A press, a pause, a drag across the monitor seam and a release, as window messages.
        let (x0, y0) = (500, 50); // client coordinates
        // SAFETY: synchronous messages to our own window on this thread.
        unsafe {
            SendMessageW(hwnd, WM_MOUSEMOVE, 0, lp(x0, y0));
            SendMessageW(hwnd, WM_LBUTTONDOWN, 1, lp(x0, y0));
            SendMessageW(hwnd, WM_MOUSEMOVE, 1, lp(x0 + 1, y0)); // within the drag threshold
            SendMessageW(hwnd, WM_MOUSEMOVE, 1, lp(700, 150));
            SendMessageW(hwnd, WM_MOUSEMOVE, 1, lp(799, 249));
            SendMessageW(hwnd, WM_LBUTTONUP, 0, lp(799, 249));
        }
        let events = take_events();
        assert_eq!(events.len(), 6, "{events:?}");
        for e in events {
            let OverlayEvent::Input(i) = e else {
                panic!("pointer messages are inputs: {e:?}")
            };
            model.handle(i);
            let next = model.scene(&win);
            let dirty = overlay::dirty(&scene, &next);
            win.present(&frozen, None, &next, &dirty);
            scene = next;
        }
        let sel = model.selection().unwrap();
        assert_eq!(
            sel,
            Rect::new(picture.x + 500, picture.y + 50, 300, 200),
            "the selection tracked the pointer in device pixels"
        );
        assert_eq!(
            model.cursor(),
            Cursor::SizeNwse,
            "the pointer rests on the corner handle"
        );
        // The back buffer: plain inside, dimmed outside, the accent border, a label box.
        let buf = win.buffer();
        let at = |x: i32, y: i32| {
            let i = ((y as usize) * 1120 + x as usize) * 4;
            [buf[i], buf[i + 1], buf[i + 2]]
        };
        let pix = |x: i32, y: i32| {
            let p = Synthetic::pixel(picture.x + x, picture.y + y);
            [p[0], p[1], p[2]]
        };
        assert_eq!(at(650, 150), pix(650, 150), "inside");
        let d = dim_pixel(Synthetic::pixel(picture.x + 1000, picture.y + 380));
        assert_eq!(
            at(1000, 380),
            [d[0], d[1], d[2]],
            "outside, on the 150 % monitor"
        );
        let a = color::ACCENT;
        assert_eq!(
            at(650, 49),
            [a[0], a[1], a[2]],
            "the border above the selection"
        );
        // The toolbar (Step 51; Step 56's tools wrap it into two rows): no room below or above
        // on the 150 % monitor, so inside the selection's bottom edge; the Pin button is
        // accent-filled and GDI drew its label.
        let bar = scene
            .toolbar
            .clone()
            .expect("a toolbar next to the selection");
        assert_eq!(bar.dpi, 144);
        assert_eq!(
            (
                bar.rect.x - picture.x,
                bar.rect.y - picture.y,
                bar.rect.width,
                bar.rect.height
            ),
            (640, 151, 471, 90),
            "two rows of buttons on the narrow 150 % monitor, inside the selection's bottom edge"
        );
        let pin = bar.buttons[0].rect;
        let (mut fill, mut ink) = (0, 0);
        for y in pin.y - picture.y..pin.y - picture.y + i32::try_from(pin.height).unwrap() {
            for x in pin.x - picture.x..pin.x - picture.x + i32::try_from(pin.width).unwrap() {
                if at(x, y) == [a[0], a[1], a[2]] {
                    fill += 1;
                } else {
                    ink += 1;
                }
            }
        }
        assert!(fill > 1000, "accent fill: {fill}");
        assert!(ink > 20, "the label's text was drawn: {ink} pixels");
        // Keys arrive as chords for the core to match.
        // SAFETY: as above.
        unsafe {
            SendMessageW(hwnd, WM_KEYDOWN, 0x0D, 0); // Enter
            SendMessageW(hwnd, WM_KEYDOWN, 0x25, 1 << 30); // Left, auto-repeat
            SendMessageW(hwnd, WM_SYSKEYDOWN, 0x73, 1 << 29); // Alt+F4
            SendMessageW(hwnd, WM_RBUTTONDOWN, 2, lp(10, 10));
            SendMessageW(hwnd, WM_CLOSE, 0, 0);
            SendMessageW(hwnd, WM_DPICHANGED, 0x0090_0090, 0);
        }
        let enter = chord_of("Enter", false, false, false).unwrap();
        let left = chord_of("Left", false, false, false).unwrap();
        let ev = take_events();
        assert_eq!(ev[0], OverlayEvent::Key(enter, None, false));
        assert_eq!(ev[1], OverlayEvent::Key(left, None, true));
        assert_eq!(ev[2], OverlayEvent::Input(Input::Leave), "Alt+F4 leaves");
        assert!(matches!(
            ev[3],
            OverlayEvent::Input(Input::Press {
                button: Button::Right,
                ..
            })
        ));
        assert_eq!(ev[4], OverlayEvent::Input(Input::Leave), "WM_CLOSE leaves");
        assert_eq!(ev.len(), 5);
        // SAFETY: as above.
        unsafe {
            let mut r = RECT::default();
            GetWindowRect(hwnd, &raw mut r);
            assert_eq!(
                (r.right - r.left, r.bottom - r.top),
                (1120, 400),
                "WM_DPICHANGED ignored"
            );
        }
        assert_eq!(win.buffer_bytes(), 1120 * 400 * 4);
        win.destroy();
        // SAFETY: a liveness query on a (now dead) handle.
        assert_eq!(unsafe { IsWindow(hwnd) }, 0, "destroyed");
        assert_eq!(win.buffer_bytes(), 0, "back buffer freed");
        win.destroy(); // idempotent
        drop(frozen);
        assert_eq!(ledger.live(), (0, 0));
    }

    #[test]
    fn system_values_are_the_users() {
        let v = system_values();
        assert!(v.drag.0 >= 1 && v.drag.1 >= 1);
        assert!((100..=5000).contains(&v.double_click_ms), "{v:?}");
    }

    /// Step 52 in the real (off-screen, never activated) window: the magnifier on the 150 %
    /// monitor is drawn into the back buffer with exact pixels and GDI's readout; key messages
    /// carry their physical position from the scan code (so the key at W's place is `code:KeyW`
    /// whatever letter the layout gives it); and a pointer key never moves the user's cursor
    /// under the test placement.
    #[test]
    fn the_real_window_draws_the_magnifier_reports_key_positions_and_keeps_the_cursor() {
        use crate::overlay::{MAG_CELLS, Mods};
        crate::win::per_monitor_dpi_awareness();
        let syn = Synthetic::off_screen();
        let desk = syn.desktop();
        let picture = desk.virtual_rect.unwrap();
        let ledger = Ledger::new();
        let frozen = syn.capture(&ledger, picture).unwrap();
        let mut win = WinOverlay::create(picture, Placement::OFF_SCREEN_TEST, false).unwrap();
        let hwnd = win.hwnd();
        let mut model = Overlay::new(
            picture,
            desk.monitors,
            SystemValues::default(),
            (picture.x + 200, picture.y + 100),
            None,
            None,
        );
        let at = (picture.x + 800, picture.y + 100); // on the 150 % monitor
        model.handle(Input::Move {
            x: at.0,
            y: at.1,
            mods: Mods::NONE,
        });
        let mut scene = model.scene(&win);
        scene.sample(&frozen);
        win.present(&frozen, None, &scene, &[picture]);
        let m = scene.magnifier.clone().expect("the magnifier");
        assert_eq!(m.cell, 12, "8 logical pixels at 150 %");
        assert_eq!(
            m.text.lines.len(),
            4,
            "position, logical position, HEX, RGB"
        );
        let buf = win.buffer();
        let px = |x: i32, y: i32| {
            let i = (((y - picture.y) as usize) * 1120 + (x - picture.x) as usize) * 4;
            [buf[i], buf[i + 1], buf[i + 2]]
        };
        let c = m.centre();
        let want = Synthetic::pixel(at.0, at.1);
        assert_eq!(
            px(c.x + 5, c.y + 5),
            [want[0], want[1], want[2]],
            "the pointer's pixel"
        );
        let corner = Synthetic::pixel(at.0 - (MAG_CELLS / 2) as i32, at.1 - (MAG_CELLS / 2) as i32);
        assert_eq!(
            px(m.grid.x + 3, m.grid.y + 3),
            [corner[0], corner[1], corner[2]],
            "the top-left cell"
        );
        // GDI drew the readout's text: the box has pixels other than its background.
        let t = m.text.rect;
        let bg = color::BG_OVERLAY;
        let mut ink = 0;
        for y in t.y..t.y + i32::try_from(t.height).unwrap() {
            for x in t.x..t.x + i32::try_from(t.width).unwrap() {
                if px(x, y) != [bg[0], bg[1], bg[2]] {
                    ink += 1;
                }
            }
        }
        assert!(ink > 50, "the readout's text was drawn: {ink} pixels");
        // Key positions from the scan code (bits 16–23 of lParam).
        let _ = take_events();
        // SAFETY: synchronous messages to our own window on this thread.
        unsafe {
            SendMessageW(hwnd, WM_KEYDOWN, 0x57, 0x11 << 16); // W at W's place (QWERTY)
            SendMessageW(hwnd, WM_KEYDOWN, 0x5A, (0x11 << 16) | (1 << 30)); // Z there (AZERTY), repeat
            SendMessageW(hwnd, WM_KEYDOWN, 0x20, 0x39 << 16); // Space: no `code:` name
        }
        let ev = take_events();
        let code_w = chord_of("code:KeyW", false, false, false);
        assert_eq!(
            ev,
            [
                OverlayEvent::Key(chord_of("W", false, false, false).unwrap(), code_w, false),
                OverlayEvent::Key(chord_of("Z", false, false, false).unwrap(), code_w, true),
                OverlayEvent::Key(chord_of("Space", false, false, false).unwrap(), None, false),
            ]
        );
        // The test placement never moves the user's cursor.
        let before = cursor_pos();
        win.warp_pointer((picture.x + 5, picture.y + 5));
        assert_eq!(cursor_pos(), before, "the cursor did not move");
        win.destroy();
        drop(frozen);
        assert_eq!(ledger.live(), (0, 0));
    }

    /// Step 56 in a real (off-screen, never activated) window: the pin editor's layered window is
    /// transparent around the pin and opaque under its toolbar and labels (GDI text included);
    /// typed characters (a surrogate pair joined), IME messages and the wheel become events; the
    /// input method is associated with the window only while asked (the user's input method is
    /// never switched — a test stand-in answers for the composition strings).
    #[test]
    fn the_layered_editor_is_clear_around_the_pin_and_ime_messages_become_events() {
        use crate::annotation::{Bounds, Object, PALETTE, Shape};
        use windows_sys::Win32::UI::Input::Ime::ImmReleaseContext;
        crate::win::per_monitor_dpi_awareness();
        let (ox, oy) = crate::capture::OFF_SCREEN;
        let canvas = Rect::new(ox, oy, 700, 300);
        let pin = Rect::new(ox + 10, oy + 40, 200, 100);
        let ledger = Ledger::new();
        let mut pixels = vec![0u8; canvas.bgra_bytes() as usize];
        for y in 40..140usize {
            for x in 10..210usize {
                let i = (y * 700 + x) * 4;
                pixels[i..i + 4].copy_from_slice(&[40, 80, 120, 255]);
            }
        }
        let base = Frame::new(&ledger, canvas, pixels).unwrap();
        let obj = Object {
            id: 1,
            shape: Shape::Rectangle(Bounds::of_rect(pin)),
            color: PALETTE[0].1,
            level: 0,
            dpi: 96,
        };
        let mut model = Overlay::for_pin(
            canvas,
            vec![crate::capture::Monitor::new(
                Rect::new(ox, oy, 800, 600),
                true,
            )],
            SystemValues::default(),
            pin,
            vec![obj.clone()],
        );
        let mut win = WinOverlay::create(canvas, Placement::OFF_SCREEN_TEST, true).unwrap();
        let hwnd = win.hwnd();
        // SAFETY: a plain query.
        let before = unsafe { GetForegroundWindow() };
        // SAFETY: plain queries on our own live window.
        unsafe {
            let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
            assert_ne!(ex & WS_EX_LAYERED, 0, "layered");
            assert_ne!(ex & WS_EX_TOOLWINDOW, 0);
            assert_eq!(ex & WS_EX_TOPMOST, 0);
            assert_ne!(ex & WS_EX_NOACTIVATE, 0);
        }
        let layer = crate::annotation::flatten(&base, &[obj], pin).unwrap();
        model.typeset(&win);
        let scene = model.scene(&win);
        win.present(&base, Some(&layer), &scene, &[canvas]);
        assert!(win.show(true));
        // SAFETY: a plain query.
        let after = unsafe { GetForegroundWindow() };
        assert_eq!(after, before, "focus never moved");
        let buf = win.buffer();
        let at = |x: i32, y: i32| {
            let i = ((y as usize) * 700 + x as usize) * 4;
            [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
        };
        let red = PALETTE[0].1;
        assert_eq!(at(10, 60), [red.b, red.g, red.r, 255], "the annotation");
        assert_eq!(at(100, 90), [40, 80, 120, 255], "the picture");
        assert_eq!(at(650, 20), [0, 0, 0, 0], "clear around the pin");
        let bar = scene.toolbar.clone().unwrap();
        let mut opaque = true;
        for y in bar.rect.y..bar.rect.y + i32::try_from(bar.rect.height).unwrap() {
            for x in bar.rect.x..bar.rect.x + i32::try_from(bar.rect.width).unwrap() {
                opaque &= at(x - ox, y - oy)[3] == 255;
            }
        }
        assert!(
            opaque,
            "the toolbar (and the text GDI drew on it) is opaque"
        );
        // Characters, IME messages and the wheel, sent to our own window.
        let _ = take_events();
        IME_TEST.with(|t| *t.borrow_mut() = Some(("\u{d55c}".into(), String::new())));
        // SAFETY: synchronous messages to our own window on this thread.
        unsafe {
            SendMessageW(hwnd, WM_CHAR, 0x61, 0);
            SendMessageW(hwnd, WM_CHAR, 0xD83D, 0);
            SendMessageW(hwnd, WM_CHAR, 0xDE00, 0);
            SendMessageW(hwnd, WM_IME_STARTCOMPOSITION, 0, 0);
            SendMessageW(hwnd, WM_IME_COMPOSITION, 0, GCS_COMPSTR as LPARAM);
        }
        IME_TEST.with(|t| *t.borrow_mut() = Some((String::new(), "\u{d55c}".into())));
        // SAFETY: as above.
        unsafe {
            SendMessageW(hwnd, WM_IME_COMPOSITION, 0, GCS_RESULTSTR as LPARAM);
            SendMessageW(hwnd, WM_IME_ENDCOMPOSITION, 0, 0);
            SendMessageW(hwnd, WM_IME_CHAR, 0xAE00, 0);
            SendMessageW(hwnd, WM_MOUSEWHEEL, (120usize) << 16, 0);
        }
        IME_TEST.with(|t| *t.borrow_mut() = None);
        assert_eq!(
            take_events(),
            [
                OverlayEvent::Char('a', false),
                OverlayEvent::Char('\u{1F600}', false),
                OverlayEvent::Ime(Ime::Start),
                OverlayEvent::Ime(Ime::Compose("\u{d55c}".into())),
                OverlayEvent::Ime(Ime::Commit("\u{d55c}".into())),
                OverlayEvent::Ime(Ime::End),
                OverlayEvent::Char('\u{ae00}', false),
                OverlayEvent::Wheel(120),
            ]
        );
        // The input method: associated while a label is edited, removed afterwards.
        let has_context = || {
            // SAFETY: the input context of our own window, released at once.
            unsafe {
                let h = ImmGetContext(hwnd);
                if !h.is_null() {
                    ImmReleaseContext(hwnd, h);
                }
                !h.is_null()
            }
        };
        assert!(
            !has_context(),
            "off by default (shortcuts work in Hangul mode)"
        );
        win.set_ime(Some(Rect::new(ox + 20, oy + 50, 2, 20)));
        assert!(has_context(), "on for the label");
        win.set_ime(None);
        assert!(!has_context(), "off again");
        // The window's typesetter has the fallback faces.
        let g = Typesetter::typeset(&win, "\u{d55c}\u{ae00}", 24);
        assert!(g.mask.iter().filter(|v| **v > 64).count() > 50);
        win.destroy();
        // SAFETY: a liveness query on a (now dead) handle.
        assert_eq!(unsafe { IsWindow(hwnd) }, 0);
        drop(layer);
        drop(base);
        assert_eq!(ledger.live(), (0, 0));
    }
}
