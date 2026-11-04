//! The floating UI on Windows (ADR 0045, docs/SUBAK_GUI.md §11): one window thread with
//!
//! - **the floating icon**: a 40 × 40 (logical) layered popup, `WS_EX_TOOLWINDOW` (no taskbar
//!   button, not in Alt+Tab), `WS_EX_NOACTIVATE` (a click never takes the focus from the window
//!   being typed into), topmost while pinned; its picture is drawn once per state change and
//!   handed to `UpdateLayeredWindow` — nothing animates and no timer runs;
//! - **the tray icon** behind [`Tray`]: `Shell_NotifyIconW` ([`ShellTray`]), or the recording
//!   [`FakeTray`] in the test mode;
//! - **the Terminal manager**: a popup made of the system's own controls — an edit field for the
//!   search, an owner-drawn list box whose item strings are the rows' accessible names, owner-
//!   drawn buttons whose window texts are theirs — so keyboard focus, the IME, touch (mouse
//!   promotion) and UI Automation names come from Windows; created when it opens and destroyed
//!   when it closes, so a closed manager costs nothing;
//! - **one IPC connection** (`subak-float`, kind `gui`) through the window's network threads.
//!
//! **Focus rules.** The icon never activates. The manager takes the focus only when the user
//! opened it (a click on the icon or the tray, the tray's keyboard selection, `--manager`), which
//! Windows allows the process that received that input. Opening a Terminal hands the foreground
//! on explicitly: `AllowSetForegroundWindow` for the window process that will show it (an
//! existing window, asked over `WM_COPYDATA`, which then calls `SetForegroundWindow` for itself)
//! or for the new `subak-gui` it starts.
//!
//! **Test hooks** (`--test-offscreen`, debug builds or the `test-hooks` feature): every window
//! is placed on a synthetic monitor layout far off every real monitor (default one 1920 × 1040
//! work area at (-20000, -20000); `--test-monitors`), never activated, never given the focus or
//! the mouse capture; a real `WM_DPICHANGED` is ignored (synthetic DPI only); the tray is the
//! fake; menus are reported instead of shown; state goes to stdout as JSON lines; `WM_COPYDATA`
//! test commands drive it; windows it starts for a Terminal are test windows too.

use super::art::{self, IconState, Palette, Picture};
use super::fprefs::{FloatPrefs, Position};
use super::list::{Effect, Entry, Manager, Mode};
use super::place::{self, Monitor};
use super::tray::{self, FakeTray, Tray, TrayCall};
use super::{COPYDATA_MAX, COPYDATA_OPEN, FloatOptions};
use crate::client::Identity;
use crate::gui::Options;
use crate::gui::keys::{self, Action, Matcher, Outcome, Press, Surface, vk};
use crate::gui::keystate;
use crate::gui::layout::{Rect, px};
use crate::gui::net::{self, Inbox, Link, Msg};
use crate::gui::screens::{Choice, tokens};
use crate::gui::window::{
    attach_parent_console, colorref, high_contrast_on, monitors as real_monitors,
    system_theme_colors, wide,
};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::{ExitCode, Stdio};
use std::ptr::{null, null_mut};
use std::sync::Arc;
use std::time::{Duration, Instant};
use subak_term::Mods;
use subak_term::render::Rgb;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT,
    SIZE, WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, BeginPaint,
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, COLOR_BTNFACE, COLOR_GRAYTEXT, COLOR_HIGHLIGHT,
    COLOR_HIGHLIGHTTEXT, COLOR_WINDOW, COLOR_WINDOWTEXT, CreateBitmap, CreateCompatibleDC,
    CreateDIBSection, CreateFontW, CreatePen, CreateSolidBrush, DEFAULT_CHARSET, DEFAULT_PITCH,
    DIB_RGB_COLORS, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE,
    DT_VCENTER, DeleteDC, DeleteObject, DrawTextW, EndPaint, FW_NORMAL, FW_SEMIBOLD, FillRect,
    FrameRect, GetDC, GetSysColor, HBRUSH, HDC, HFONT, HGDIOBJ, InvalidateRect, OUT_DEFAULT_PRECIS,
    PAINTSTRUCT, PS_SOLID, RDW_ALLCHILDREN, RDW_INVALIDATE, RedrawWindow, ReleaseDC, RoundRect,
    SelectObject, SetBkColor, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows_sys::Win32::Storage::Xps::PrintWindow;
use windows_sys::Win32::System::DataExchange::COPYDATASTRUCT;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{CreateMutexW, GetCurrentProcess, GetGuiResources};
use windows_sys::Win32::UI::Controls::{
    DRAWITEMSTRUCT, EM_SETSEL, MEASUREITEMSTRUCT, ODS_FOCUS, ODS_SELECTED, ODT_BUTTON, ODT_LISTBOX,
};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, GetSystemMetricsForDpi,
    SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, ReleaseCapture, SetCapture, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, ToUnicode,
    TrackMouseEvent,
};
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION,
    NIN_SELECT, NINF_KEY, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, AppendMenuW, BN_CLICKED, BS_OWNERDRAW, CREATESTRUCTW, CS_DBLCLKS,
    CreateIconIndirect, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu,
    DestroyWindow, DispatchMessageW, EN_CHANGE, ES_AUTOHSCROLL, EnumWindows, GWL_EXSTYLE,
    GetClassNameW, GetClientRect, GetMessageW, GetWindowLongPtrW, GetWindowRect,
    GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, HICON, HWND_NOTOPMOST,
    HWND_TOPMOST, ICONINFO, IDC_ARROW, IsChild, IsIconic, IsWindowVisible, KillTimer, LB_ADDSTRING,
    LB_ERR, LB_GETCOUNT, LB_GETCURSEL, LB_GETITEMRECT, LB_GETTEXT, LB_GETTEXTLEN, LB_GETTOPINDEX,
    LB_RESETCONTENT, LB_SETCURSEL, LB_SETITEMHEIGHT, LB_SETTOPINDEX, LBN_DBLCLK, LBN_SELCHANGE,
    LBS_HASSTRINGS, LBS_NOINTEGRALHEIGHT, LBS_NOTIFY, LBS_OWNERDRAWFIXED, LoadCursorW, MF_CHECKED,
    MF_GRAYED, MF_SEPARATOR, MF_STRING, MSG, MoveWindow, PostMessageW, PostQuitMessage,
    RegisterClassExW, RegisterWindowMessageW, SM_CXDRAG, SM_CXSMICON, SMTO_ABORTIFHUNG, SW_HIDE,
    SW_RESTORE, SW_SHOW, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOZORDER, SendMessageTimeoutW,
    SendMessageW, SetForegroundWindow, SetTimer, SetWindowPos, SetWindowTextW, ShowWindow,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenuEx, TranslateMessage, ULW_ALPHA,
    UpdateLayeredWindow, WM_ACTIVATE, WM_APP, WM_CHAR, WM_CLOSE, WM_COMMAND, WM_CONTEXTMENU,
    WM_COPYDATA, WM_CTLCOLOREDIT, WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC, WM_DESTROY,
    WM_DISPLAYCHANGE, WM_DPICHANGED, WM_DRAWITEM, WM_ENDSESSION, WM_ERASEBKGND, WM_KEYDOWN,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MEASUREITEM, WM_MOUSEMOVE, WM_NULL, WM_PAINT, WM_RBUTTONUP,
    WM_SETFONT, WM_SETTINGCHANGE, WM_SYSCOLORCHANGE, WM_SYSKEYDOWN, WM_THEMECHANGED, WM_TIMER,
    WNDCLASSEXW, WS_CHILD, WS_CLIPCHILDREN, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};

/// The network threads have queued something.
const WM_NET: u32 = WM_APP + 1;
/// The tray icon's callback.
const WM_TRAY: u32 = WM_APP + 3;
/// A menu choice (`wParam` the command), posted after the menu's own loop.
const WM_MENU: u32 = WM_APP + 4;
/// Close the manager after the current message (it lost the focus, or a menu choice).
const WM_CLOSE_MANAGER: u32 = WM_APP + 5;
const TIMER_RECONNECT: usize = 1;
const TIMER_TEST_EXIT: usize = 2;
/// `dwData` of a test command (`WM_COPYDATA`): "SUBK", as the window's.
const COPYDATA_TEST: usize = 0x5355_424B;
/// The terminal windows' class (`gui::window`).
const WINDOW_CLASS: &str = "SubakWindow";
const ICON_CLASS: &str = "SubakFloat";
const MANAGER_CLASS: &str = "SubakManager";
/// `SS_NOPREFIX` | `SS_LEFT`: a static text control that shows `&` as it is.
const SS_TEXT: u32 = 0x0080;
/// `SPI_SETWORKAREA`: the work area changed (a taskbar moved).
const SPI_SETWORKAREA: usize = 0x002F;
/// The test mode's monitor layout: off every real monitor of any desktop.
const OFFSCREEN: i32 = -20000;
/// The tray icon's id.
const TRAY_ID: u32 = 1;
/// The tray icon chosen with the keyboard (`NIN_SELECT | NINF_KEY`).
const NIN_KEYSELECT: u32 = NIN_SELECT | NINF_KEY;
/// `WM_MOUSELEAVE` (the mouse left the icon; `TrackMouseEvent`).
const WM_MOUSELEAVE: u32 = 0x02A3;

// The icon's menu.
const CMD_MANAGER: u32 = 1;
const CMD_OPEN_WINDOW: u32 = 2;
const CMD_NEW: u32 = 3;
const CMD_ON_TOP: u32 = 4;
const CMD_SHOW_ICON: u32 = 5;
const CMD_SHOW_TRAY: u32 = 6;
const CMD_QUIT: u32 = 7;
// A row's menu.
const CMD_ROW_OPEN: u32 = 20;
const CMD_ROW_RENAME: u32 = 21;
const CMD_ROW_DELETE: u32 = 22;
const CMD_ROW_NEW: u32 = 23;

// The manager's controls (their ids are also the logical focus).
const ID_SEARCH: i32 = 101;
const ID_LIST: i32 = 102;
const ID_NEW: i32 = 110;
const ID_OPEN: i32 = 111;
const ID_RENAME: i32 = 112;
const ID_DELETE: i32 = 113;
const ID_PIN: i32 = 114;
const ID_CLOSE: i32 = 115;
const ID_CANCEL: i32 = 120;
const ID_END: i32 = 121;
const ID_RENAME_EDIT: i32 = 130;
const ID_TITLE: i32 = 140;
const ID_SUMMARY: i32 = 141;
const ID_SEARCH_LABEL: i32 = 142;
const ID_EMPTY: i32 = 143;
const ID_STATUS: i32 = 144;
const ID_ASK: i32 = 145;
/// The focus order (Tab) while browsing.
const FOCUS_ORDER: [i32; 8] = [
    ID_SEARCH, ID_LIST, ID_NEW, ID_OPEN, ID_RENAME, ID_DELETE, ID_PIN, ID_CLOSE,
];

/// A row's height, logical pixels (touch-friendly; DESIGN_SYSTEM control 28 + 4).
const ROW: f32 = 32.0;

fn ctl_name(id: i32) -> &'static str {
    match id {
        ID_SEARCH => "search",
        ID_LIST => "list",
        ID_NEW => "new",
        ID_OPEN => "open",
        ID_RENAME => "rename",
        ID_DELETE => "delete",
        ID_PIN => "pin",
        ID_CLOSE => "close",
        ID_CANCEL => "cancel",
        ID_END => "end",
        ID_RENAME_EDIT => "rename-field",
        ID_TITLE => "title",
        ID_SUMMARY => "summary",
        ID_SEARCH_LABEL => "search-label",
        ID_EMPTY => "empty",
        ID_STATUS => "status",
        ID_ASK => "question",
        _ => "",
    }
}

fn ctl_of(name: &str) -> Option<i32> {
    [
        ID_SEARCH,
        ID_LIST,
        ID_NEW,
        ID_OPEN,
        ID_RENAME,
        ID_DELETE,
        ID_PIN,
        ID_CLOSE,
        ID_CANCEL,
        ID_END,
        ID_RENAME_EDIT,
    ]
    .into_iter()
    .find(|i| ctl_name(*i) == name)
}

fn lparam_xy(lp: LPARAM) -> (i32, i32) {
    (
        i32::from(lp as u16 as i16),
        i32::from((lp >> 16) as u16 as i16),
    )
}

fn rect_of(r: &RECT) -> Rect {
    Rect {
        left: r.left,
        top: r.top,
        right: r.right,
        bottom: r.bottom,
    }
}

fn rect_json(r: &Rect) -> Value {
    json!([r.left, r.top, r.right, r.bottom])
}

/// The window text of a window (its UI Automation name for the system's controls).
fn text_of(hwnd: HWND) -> String {
    // SAFETY: reads a window's text into a buffer of the length it reports.
    unsafe {
        let n = GetWindowTextLengthW(hwnd);
        if n <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; n as usize + 1];
        let got = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        String::from_utf16_lossy(&buf[..usize::try_from(got).unwrap_or(0)])
    }
}

fn set_text(hwnd: HWND, s: &str) {
    if text_of(hwnd) == s {
        return;
    }
    let w = wide(s);
    // SAFETY: sets the text of one of our controls.
    unsafe { SetWindowTextW(hwnd, w.as_ptr()) };
}

fn class_of(hwnd: HWND) -> String {
    let mut buf = [0u16; 64];
    // SAFETY: a fixed buffer of the given length.
    let n = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..usize::try_from(n).unwrap_or(0)])
}

fn window_pid(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    // SAFETY: an out-parameter for the window's process id.
    unsafe { GetWindowThreadProcessId(hwnd, &raw mut pid) };
    pid
}

/// The top-level windows of a class, front to back.
fn windows_of_class(class: &str) -> Vec<HWND> {
    struct Find {
        class: String,
        found: Vec<HWND>,
    }
    unsafe extern "system" fn each(hwnd: HWND, data: LPARAM) -> windows_sys::core::BOOL {
        // SAFETY: `data` is the `Find` passed below, alive for the enumeration.
        let f = unsafe { &mut *(data as *mut Find) };
        if class_of(hwnd) == f.class {
            f.found.push(hwnd);
        }
        1
    }
    let mut f = Find {
        class: class.to_string(),
        found: Vec::new(),
    };
    // SAFETY: the callback only touches `f`, which outlives the call.
    unsafe { EnumWindows(Some(each), (&raw mut f) as LPARAM) };
    f.found
}

/// Sends a Subak request to a window of another process (or this one) and returns its answer.
fn send_copydata(hwnd: HWND, kind: usize, body: &Value) -> usize {
    let text = body.to_string();
    let cds = COPYDATASTRUCT {
        dwData: kind,
        cbData: text.len() as u32,
        lpData: text.as_ptr() as *mut _,
    };
    let mut result = 0usize;
    // SAFETY: a synchronous WM_COPYDATA with a timeout; `cds` and `text` outlive the call.
    let ok = unsafe {
        SendMessageTimeoutW(
            hwnd,
            WM_COPYDATA,
            0,
            (&raw const cds) as LPARAM,
            SMTO_ABORTIFHUNG,
            2_000,
            &raw mut result,
        )
    };
    if ok == 0 { 0 } else { result }
}

/// Parses a `WM_COPYDATA` body of a Subak request (`None`: not one, or too large).
///
/// # Safety
/// `lp` must be the `lParam` of a `WM_COPYDATA` message being handled.
pub(crate) unsafe fn copydata(lp: LPARAM, kind: usize) -> Option<Value> {
    // SAFETY: the caller passes the lParam of WM_COPYDATA, valid during the call.
    let cds = unsafe { &*(lp as *const COPYDATASTRUCT) };
    if cds.dwData != kind || cds.lpData.is_null() || cds.cbData as usize > COPYDATA_MAX * 16 {
        return None;
    }
    // SAFETY: `cbData` bytes at `lpData`, as the sender described them.
    let bytes = unsafe { std::slice::from_raw_parts(cds.lpData.cast::<u8>(), cds.cbData as usize) };
    if kind == COPYDATA_OPEN && bytes.len() > COPYDATA_MAX {
        return None;
    }
    serde_json::from_slice(bytes).ok()
}

/// The character a key produces on the current layout with Shift as held (for `char:` keys),
/// without changing the dead-key state.
fn produced_char(vk_code: u32, scan: u32, shift: bool) -> Option<char> {
    let mut state = [0u8; 256];
    if shift {
        state[vk::SHIFT as usize] = 0x80;
    }
    let mut buf = [0u16; 8];
    // SAFETY: a 256-byte key state and a buffer of the given length.
    let n = unsafe { ToUnicode(vk_code, scan, state.as_ptr(), buf.as_mut_ptr(), 8, 4) };
    (n == 1)
        .then(|| char::from_u32(u32::from(buf[0])))
        .flatten()
}

/// Shift, Ctrl and Alt for the message being processed (none from the real keyboard in the
/// off-screen test mode, [`keystate`]).
fn held() -> Mods {
    keystate::held()
}

// ---------------------------------------------------------------------------------------------
// The tray (Shell_NotifyIconW).

/// A tray icon in the notification area. Never used by the test mode.
#[derive(Debug)]
struct ShellTray {
    hwnd: HWND,
    shown: bool,
    icon: HICON,
}

/// An icon handle from a picture (straight alpha, as icons take it).
pub(in crate::gui) fn hicon(p: &Picture) -> HICON {
    let size = p.size as i32;
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size,
            biHeight: -size,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..BITMAPINFOHEADER::default()
        },
        ..BITMAPINFO::default()
    };
    // SAFETY: a DIB section of the picture's size, filled within its bounds; the mask and the
    // colour bitmap are deleted once the icon owns copies of them.
    unsafe {
        let dc = GetDC(null_mut());
        let mut bits: *mut core::ffi::c_void = null_mut();
        let color = CreateDIBSection(
            dc,
            &raw const bmi,
            DIB_RGB_COLORS,
            &raw mut bits,
            null_mut(),
            0,
        );
        ReleaseDC(null_mut(), dc);
        if color.is_null() || bits.is_null() {
            return null_mut();
        }
        let out = std::slice::from_raw_parts_mut(bits.cast::<u32>(), p.pixels.len());
        for y in 0..p.size {
            for x in 0..p.size {
                let (r, g, b, a) = p.rgba(x, y);
                out[(y * p.size + x) as usize] = (u32::from(a) << 24)
                    | (u32::from(r) << 16)
                    | (u32::from(g) << 8)
                    | u32::from(b);
            }
        }
        let mask_bits = vec![0u8; ((p.size as usize).div_ceil(16) * 2) * p.size as usize];
        let mask = CreateBitmap(size, size, 1, 1, mask_bits.as_ptr().cast());
        let ii = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&raw const ii);
        DeleteObject(mask as HGDIOBJ);
        DeleteObject(color as HGDIOBJ);
        icon
    }
}

impl ShellTray {
    fn data(&self, tip: &str, icon: HICON) -> NOTIFYICONDATAW {
        let mut d = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: TRAY_ID,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
            uCallbackMessage: WM_TRAY,
            hIcon: icon,
            ..NOTIFYICONDATAW::default()
        };
        for (i, u) in tray::tip(tip).encode_utf16().take(127).enumerate() {
            d.szTip[i] = u;
        }
        d
    }

    fn set_icon(&mut self, p: &Picture) -> HICON {
        let next = hicon(p);
        if !self.icon.is_null() {
            // SAFETY: our own icon, no longer used once replaced.
            unsafe { DestroyIcon(self.icon) };
        }
        self.icon = next;
        next
    }
}

impl Tray for ShellTray {
    fn add(&mut self, tip: &str, picture: &Picture) -> Result<(), String> {
        let icon = self.set_icon(picture);
        let mut d = self.data(tip, icon);
        // SAFETY: a correctly sized NOTIFYICONDATAW for our own window.
        let ok = unsafe { Shell_NotifyIconW(NIM_ADD, &raw const d) } != 0;
        if !ok {
            return Err("the notification area refused the icon".into());
        }
        d.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        // SAFETY: as above.
        unsafe { Shell_NotifyIconW(NIM_SETVERSION, &raw const d) };
        self.shown = true;
        Ok(())
    }

    fn modify(&mut self, tip: &str, picture: &Picture) -> Result<(), String> {
        if !self.shown {
            return Err("not shown".into());
        }
        let icon = self.set_icon(picture);
        let d = self.data(tip, icon);
        // SAFETY: as in `add`.
        let ok = unsafe { Shell_NotifyIconW(NIM_MODIFY, &raw const d) } != 0;
        ok.then_some(())
            .ok_or_else(|| "the notification area refused the change".into())
    }

    fn delete(&mut self) {
        if self.shown {
            let d = self.data("", self.icon);
            // SAFETY: as in `add`.
            unsafe { Shell_NotifyIconW(NIM_DELETE, &raw const d) };
            self.shown = false;
        }
        if !self.icon.is_null() {
            // SAFETY: our own icon.
            unsafe { DestroyIcon(self.icon) };
            self.icon = null_mut();
        }
    }

    fn shown(&self) -> bool {
        self.shown
    }
}

// ---------------------------------------------------------------------------------------------
// State.

/// A Win32 call to make once the state is no longer borrowed (it sends messages back, or runs a
/// modal loop, or talks to another process).
#[derive(Debug)]
enum After {
    /// `SetWindowPos`: position, size, z-order (`Some(true)`: topmost).
    Place {
        hwnd: HWND,
        rect: Rect,
        topmost: Option<bool>,
    },
    /// Hand the icon's picture to `UpdateLayeredWindow` at a position.
    Present(Picture, (i32, i32)),
    Show(HWND, i32),
    Focus(HWND),
    Foreground(HWND),
    /// Close the manager: its window, then its fonts and brushes.
    CloseUi(Box<Ui>),
    /// A native popup menu at a screen point; its choice comes back as `WM_MENU`.
    Menu {
        owner: HWND,
        items: Vec<(u32, String, u32)>,
        at: (i32, i32),
    },
    /// Show a Terminal in a window (an existing one asked over WM_COPYDATA, or a new one), then
    /// close the manager (when it is not pinned).
    Open(Option<Entry>, bool),
    /// Quit: tray removed, windows destroyed, the message loop ends.
    Quit,
    /// The test hook's picture of the manager (`PrintWindow`, its own content only).
    Shot {
        hwnd: HWND,
        path: String,
    },
}

/// A press on the icon that may become a drag.
#[derive(Debug, Clone, Copy)]
struct Drag {
    /// The cursor's screen position at the press, and the icon's origin then.
    from: (i32, i32),
    origin: (i32, i32),
    moved: bool,
}

/// The fonts and brushes the manager draws with, at its DPI and theme.
#[derive(Debug)]
struct Paint {
    body: HFONT,
    title: HFONT,
    symbol: HFONT,
    bg: HBRUSH,
    selected: HBRUSH,
    border: HBRUSH,
    field: HBRUSH,
    hc: bool,
}

impl Paint {
    fn colors(hc: bool) -> Colors {
        if hc {
            // SAFETY: plain system colour queries.
            let s = |i| crate::gui::window::rgb_of(unsafe { GetSysColor(i) });
            Colors {
                bg: s(COLOR_WINDOW),
                text: s(COLOR_WINDOWTEXT),
                muted: s(COLOR_GRAYTEXT),
                selected: s(COLOR_HIGHLIGHT),
                selected_text: s(COLOR_HIGHLIGHTTEXT),
                border: s(COLOR_WINDOWTEXT),
                accent: s(COLOR_HIGHLIGHT),
                on_accent: s(COLOR_HIGHLIGHTTEXT),
                danger: s(COLOR_WINDOWTEXT),
                field: s(COLOR_BTNFACE),
                edge: s(COLOR_WINDOWTEXT),
                focus: s(COLOR_HIGHLIGHT),
            }
        } else {
            Colors {
                bg: tokens::BG_OVERLAY,
                text: tokens::TEXT_PRIMARY,
                muted: tokens::TEXT_MUTED,
                selected: tokens::BG_SELECTED,
                selected_text: tokens::TEXT_PRIMARY,
                border: tokens::BORDER_SUBTLE,
                accent: tokens::ACCENT,
                on_accent: tokens::BG_BASE,
                danger: tokens::DANGER,
                field: tokens::BG_RAISED,
                edge: tokens::BORDER_STRONG,
                focus: tokens::FOCUS_RING,
            }
        }
    }

    fn new(dpi: u32, hc: bool) -> Self {
        let font = |size: f32, weight: i32, face: &str| {
            let f = wide(face);
            // SAFETY: creates a GDI font; deleted in `drop`.
            unsafe {
                CreateFontW(
                    -px(size, dpi),
                    0,
                    0,
                    0,
                    weight,
                    0,
                    0,
                    0,
                    u32::from(DEFAULT_CHARSET),
                    u32::from(OUT_DEFAULT_PRECIS),
                    u32::from(CLIP_DEFAULT_PRECIS),
                    u32::from(CLEARTYPE_QUALITY),
                    u32::from(DEFAULT_PITCH),
                    f.as_ptr(),
                )
            }
        };
        let c = Self::colors(hc);
        // SAFETY: solid brushes, deleted in `drop`.
        let brush = |rgb: Rgb| unsafe { CreateSolidBrush(colorref(rgb)) };
        Self {
            body: font(12.0, FW_NORMAL as i32, "Segoe UI"),
            title: font(15.0, FW_SEMIBOLD as i32, "Segoe UI"),
            symbol: font(12.0, FW_NORMAL as i32, "Segoe UI Symbol"),
            bg: brush(c.bg),
            selected: brush(c.selected),
            border: brush(c.border),
            field: brush(c.field),
            hc,
        }
    }
}

impl Drop for Paint {
    fn drop(&mut self) {
        // SAFETY: our own GDI objects, no longer selected anywhere.
        unsafe {
            for o in [
                self.body as HGDIOBJ,
                self.title as HGDIOBJ,
                self.symbol as HGDIOBJ,
                self.bg as HGDIOBJ,
                self.selected as HGDIOBJ,
                self.border as HGDIOBJ,
                self.field as HGDIOBJ,
            ] {
                DeleteObject(o);
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Colors {
    bg: Rgb,
    text: Rgb,
    muted: Rgb,
    selected: Rgb,
    selected_text: Rgb,
    border: Rgb,
    accent: Rgb,
    on_accent: Rgb,
    danger: Rgb,
    field: Rgb,
    /// Input and control boundaries (WCAG 1.4.11: 3:1).
    edge: Rgb,
    focus: Rgb,
}

/// The open Terminal manager.
#[derive(Debug)]
struct Ui {
    hwnd: HWND,
    dpi: u32,
    paint: Paint,
    /// Child controls by id.
    ctls: Vec<(i32, HWND)>,
    /// The rename field, while renaming.
    rename: Option<HWND>,
    /// The logical focus (the control keys go to; in the test mode it is never the system's).
    focus: i32,
    /// The list box's item strings as last filled (rebuilt only when they change).
    items: Vec<String>,
    /// The status line's notice.
    notice: Option<String>,
    /// Programmatic changes in progress (no EN_CHANGE/LBN_SELCHANGE feedback).
    syncing: bool,
    opened: Instant,
}

impl Ui {
    fn ctl(&self, id: i32) -> HWND {
        if id == ID_RENAME_EDIT {
            return self.rename.unwrap_or(null_mut());
        }
        self.ctls
            .iter()
            .find(|(i, _)| *i == id)
            .map_or(null_mut(), |(_, h)| *h)
    }

    fn id_of(&self, hwnd: HWND) -> Option<i32> {
        if self.rename == Some(hwnd) {
            return Some(ID_RENAME_EDIT);
        }
        self.ctls.iter().find(|(_, h)| *h == hwnd).map(|(i, _)| *i)
    }
}

#[derive(Debug)]
struct Float {
    opts: Options,
    fopts: FloatOptions,
    endpoint: String,
    prefs: FloatPrefs,
    prefs_dir: Option<PathBuf>,
    prefs_read_only: bool,
    icon: HWND,
    pos: (i32, i32),
    dpi: u32,
    drag: Option<Drag>,
    hovering: bool,
    test_monitors: Option<Vec<Monitor>>,
    tray: Box<dyn TrayDebug>,
    /// Where the tray was last clicked (the manager opens near it when the icon is hidden).
    tray_anchor: Option<(i32, i32)>,
    model: Manager,
    link: Option<Link>,
    generation: u64,
    connecting: bool,
    inbox: Arc<Inbox>,
    matcher: Matcher,
    ui: Option<Ui>,
    hc: bool,
    shown_state: Option<(IconState, u32, bool)>,
    picture: Option<Picture>,
    renders: u64,
    taskbar_created: u32,
    notice: Option<String>,
    after: Vec<After>,
    opened: Vec<Value>,
}

/// A tray that can be printed (the state is `Debug`).
trait TrayDebug: Tray + std::fmt::Debug {}
impl<T: Tray + std::fmt::Debug> TrayDebug for T {}

thread_local! {
    static FLOAT: RefCell<Option<Float>> = const { RefCell::new(None) };
}

impl Float {
    fn event(&self, v: &Value) {
        if self.opts.test {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{v}");
            let _ = out.flush();
        }
    }

    fn monitors(&self) -> Vec<Monitor> {
        if let Some(m) = &self.test_monitors {
            return m.clone();
        }
        real_monitors()
            .into_iter()
            .map(|(work, dpi)| Monitor { work, dpi })
            .collect()
    }

    /// The test mode never puts a window where it could be seen.
    fn allowed(&self, r: &Rect) -> bool {
        !self.opts.test || r.right <= -10_000 || r.bottom <= -10_000
    }

    fn size(&self) -> i32 {
        place::icon_size(self.dpi)
    }

    fn icon_rect(&self) -> Rect {
        let s = self.size();
        Rect {
            left: self.pos.0,
            top: self.pos.1,
            right: self.pos.0 + s,
            bottom: self.pos.1 + s,
        }
    }

    fn palette(&self) -> Palette {
        if self.hc {
            let (w, t, h) = system_theme_colors();
            Palette::high_contrast(w, t, h)
        } else {
            Palette::subak()
        }
    }

    fn save_prefs(&mut self) {
        let Some(dir) = self.prefs_dir.clone() else {
            return;
        };
        if self.prefs_read_only {
            return;
        }
        match self.prefs.save(&dir) {
            Ok(()) => self.event(&json!({"event": "saved", "path": dir.join(super::fprefs::FILE)})),
            Err(e) => self.event(&json!({"event": "save_failed", "error": e.to_string()})),
        }
    }

    // -----------------------------------------------------------------------------------------
    // The icon and the tray.

    /// Redraws the icon and the tray when their state changed (never while nothing changed).
    fn refresh_icon(&mut self, force: bool) {
        let state = self.model.icon_state();
        let key = (state, self.dpi, self.hc);
        if !force && self.shown_state == Some(key) {
            return;
        }
        self.shown_state = Some(key);
        let pal = self.palette();
        let tip = art::tooltip(&state);
        set_text(self.icon, &tip);
        let pic = art::render(&state, self.size() as u32, &pal);
        self.renders += 1;
        if self.prefs.icon && self.allowed(&self.icon_rect()) {
            self.after.push(After::Present(pic.clone(), self.pos));
        }
        if self.prefs.tray {
            // SAFETY: a plain metrics query.
            let small = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, self.dpi) }.clamp(16, 64);
            let tp = art::render(&state, small as u32, &pal);
            let r = if self.tray.shown() {
                self.tray.modify(&tip, &tp)
            } else {
                self.tray.add(&tip, &tp)
            };
            if let Err(e) = r {
                self.event(&json!({"event": "tray_failed", "error": e}));
            }
        } else if self.tray.shown() {
            self.tray.delete();
        }
        self.picture = Some(pic);
        self.event(
            &json!({"event": "icon", "badge": art::badge(&state).name(), "tip": tip,
            "conn": self.model.conn.name()}),
        );
    }

    /// Shows or hides the floating icon and the tray as the settings say; quits when neither.
    fn apply_presence(&mut self) {
        if !self.prefs.icon && !self.prefs.tray {
            self.event(&json!({"event": "quitting", "why": "neither the icon nor the tray is on"}));
            self.after.push(After::Quit);
            return;
        }
        self.after.push(After::Show(
            self.icon,
            if self.prefs.icon {
                SW_SHOWNOACTIVATE
            } else {
                SW_HIDE
            },
        ));
        self.after.push(After::Place {
            hwnd: self.icon,
            rect: self.icon_rect(),
            topmost: Some(self.prefs.on_top),
        });
        self.refresh_icon(true);
    }

    fn move_icon(&mut self, pos: (i32, i32), dpi: u32) {
        let resized = dpi != self.dpi;
        self.pos = pos;
        self.dpi = dpi;
        let r = self.icon_rect();
        if !self.allowed(&r) {
            self.event(&json!({"event": "error", "message": "a test window may not be placed on screen", "rect": rect_json(&r)}));
            return;
        }
        if resized {
            self.refresh_icon(true);
        } else {
            self.after.push(After::Place {
                hwnd: self.icon,
                rect: r,
                topmost: None,
            });
        }
    }

    /// Monitors changed (`WM_DISPLAYCHANGE`, the work area, a test layout): an icon that is no
    /// longer fully on a monitor comes back onto the nearest one. The saved place is kept, so it
    /// is used again when its monitor returns.
    fn on_display_change(&mut self) {
        let mons = self.monitors();
        let s = self.size();
        if place::fully_visible(self.pos.0, self.pos.1, s, &mons) {
            self.event(
                &json!({"event": "display", "moved": false, "pos": [self.pos.0, self.pos.1]}),
            );
            return;
        }
        if let Some((pos, dpi)) = place::restore(self.pos.0, self.pos.1, self.dpi, &mons) {
            self.move_icon(pos, dpi);
            self.event(
                &json!({"event": "display", "moved": true, "pos": [pos.0, pos.1], "dpi": dpi}),
            );
        }
        if self.ui.is_some() {
            self.place_manager();
        }
    }

    fn on_dpi(&mut self, dpi: u32, suggested: Option<(i32, i32)>) {
        let pos = suggested.unwrap_or(self.pos);
        self.move_icon(pos, dpi.max(48));
        self.refresh_icon(true);
    }

    fn on_mouse(&mut self, msg: u32, wp: WPARAM, x: i32, y: i32) {
        let screen = (self.pos.0 + x, self.pos.1 + y);
        match msg {
            WM_LBUTTONDOWN => {
                self.drag = Some(Drag {
                    from: screen,
                    origin: self.pos,
                    moved: false,
                });
                if !self.opts.test {
                    // SAFETY: our own window takes the mouse while the button is down.
                    unsafe { SetCapture(self.icon) };
                }
            }
            WM_MOUSEMOVE => {
                if !self.hovering {
                    self.hovering = true;
                    if !self.opts.test {
                        let mut t = TRACKMOUSEEVENT {
                            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE,
                            hwndTrack: self.icon,
                            dwHoverTime: 0,
                        };
                        // SAFETY: a correctly sized structure for our own window.
                        unsafe { TrackMouseEvent(&raw mut t) };
                    }
                    // Looking at the icon: retry a missing Host at once.
                    if !self.model.connected() && !self.connecting {
                        let fx = self.model.refresh();
                        self.apply(fx);
                    }
                }
                let Some(mut d) = self.drag else {
                    return;
                };
                if wp & 0x0001 == 0 {
                    self.drag = None; // MK_LBUTTON released elsewhere
                    return;
                }
                // SAFETY: a plain metrics query.
                let threshold = unsafe { GetSystemMetricsForDpi(SM_CXDRAG, self.dpi) }.max(1);
                if !d.moved && place::is_drag(d.from, screen, threshold) {
                    d.moved = true;
                    self.close_manager();
                }
                if d.moved {
                    let (pos, dpi) =
                        place::drag_to(d.origin, d.from, screen, self.size(), &self.monitors());
                    self.move_icon(pos, dpi);
                }
                self.drag = Some(d);
            }
            WM_LBUTTONUP => {
                if !self.opts.test {
                    // SAFETY: releases our own capture.
                    unsafe { ReleaseCapture() };
                }
                let Some(d) = self.drag.take() else {
                    return;
                };
                if d.moved {
                    self.prefs.position = Some(Position {
                        x: self.pos.0,
                        y: self.pos.1,
                        dpi: self.dpi,
                    });
                    self.save_prefs();
                    self.event(&json!({"event": "dragged", "pos": [self.pos.0, self.pos.1], "dpi": self.dpi}));
                } else {
                    self.event(&json!({"event": "clicked"}));
                    self.toggle_manager(None);
                }
            }
            WM_RBUTTONUP => self.icon_menu(screen),
            _ => {}
        }
    }

    fn icon_menu(&mut self, at: (i32, i32)) {
        let check = |b: bool| if b { MF_CHECKED } else { 0 };
        let items = vec![
            (CMD_MANAGER, "Terminals…".to_string(), 0),
            (CMD_OPEN_WINDOW, "Open Subak window".to_string(), 0),
            (CMD_NEW, "New Terminal".to_string(), 0),
            (0, String::new(), 0),
            (
                CMD_ON_TOP,
                "Always on top".to_string(),
                check(self.prefs.on_top),
            ),
            (
                CMD_SHOW_ICON,
                "Show floating icon".to_string(),
                check(self.prefs.icon),
            ),
            (
                CMD_SHOW_TRAY,
                "Show tray icon".to_string(),
                check(self.prefs.tray),
            ),
            (0, String::new(), 0),
            (CMD_QUIT, "Quit (Terminals keep running)".to_string(), 0),
        ];
        self.menu(self.icon, items, at);
    }

    fn menu(&mut self, owner: HWND, items: Vec<(u32, String, u32)>, at: (i32, i32)) {
        if self.opts.test {
            // A test never shows a menu (it would appear on the real desktop).
            self.event(&json!({"event": "menu", "items": items.iter()
                .map(|(id, l, f)| json!({"id": id, "label": l, "checked": f & MF_CHECKED != 0,
                    "enabled": f & MF_GRAYED == 0}))
                .collect::<Vec<_>>()}));
            return;
        }
        self.after.push(After::Menu { owner, items, at });
    }

    fn menu_command(&mut self, cmd: u32) {
        self.event(&json!({"event": "command", "id": cmd}));
        match cmd {
            CMD_MANAGER => self.open_manager(None),
            CMD_OPEN_WINDOW => self
                .after
                .push(After::Open(None, !self.prefs.manager_pinned)),
            CMD_NEW | CMD_ROW_NEW => {
                let fx = self.model.create();
                self.apply(fx);
            }
            CMD_ON_TOP => {
                self.prefs.on_top = !self.prefs.on_top;
                self.save_prefs();
                self.after.push(After::Place {
                    hwnd: self.icon,
                    rect: self.icon_rect(),
                    topmost: Some(self.prefs.on_top),
                });
                if let Some(ui) = &self.ui {
                    let mut r = RECT::default();
                    // SAFETY: our own window's rectangle.
                    unsafe { GetWindowRect(ui.hwnd, &raw mut r) };
                    self.after.push(After::Place {
                        hwnd: ui.hwnd,
                        rect: rect_of(&r),
                        topmost: Some(self.prefs.on_top),
                    });
                }
            }
            CMD_SHOW_ICON => {
                self.prefs.icon = !self.prefs.icon;
                self.save_prefs();
                self.apply_presence();
            }
            CMD_SHOW_TRAY => {
                self.prefs.tray = !self.prefs.tray;
                self.save_prefs();
                self.apply_presence();
            }
            CMD_QUIT => self.after.push(After::Quit),
            CMD_ROW_OPEN => self.run(Action::ManagerOpen),
            CMD_ROW_RENAME => self.run(Action::ManagerRename),
            CMD_ROW_DELETE => self.run(Action::ManagerDelete),
            _ => {}
        }
    }

    fn on_tray(&mut self, wp: WPARAM, lp: LPARAM) {
        let event = (lp & 0xFFFF) as u32;
        let at = (
            i32::from(wp as u16 as i16),
            i32::from((wp >> 16) as u16 as i16),
        );
        self.event(&json!({"event": "tray_event", "code": event}));
        match event {
            NIN_SELECT | NIN_KEYSELECT | WM_LBUTTONUP => {
                self.tray_anchor = Some(at);
                self.toggle_manager(Some(at));
            }
            WM_CONTEXTMENU | WM_RBUTTONUP => {
                self.tray_anchor = Some(at);
                self.icon_menu(at);
            }
            _ => {}
        }
    }

    // -----------------------------------------------------------------------------------------
    // The Host.

    fn connect(&mut self, start: bool) {
        self.generation += 1;
        self.connecting = true;
        if let Some(l) = self.link.take() {
            l.close();
        }
        net::connect_as(
            self.endpoint.clone(),
            start,
            self.generation,
            Arc::clone(&self.inbox),
            Identity::FLOAT,
        );
    }

    fn on_net(&mut self) {
        for (generation, m) in self.inbox.drain() {
            if generation != self.generation {
                if let Msg::Connected { link, .. } = m {
                    link.close();
                }
                continue;
            }
            let fx = match m {
                Msg::Connected {
                    link,
                    host_pid,
                    started,
                } => {
                    self.connecting = false;
                    self.link = Some(link);
                    self.event(
                        &json!({"event": "connected", "host_pid": host_pid, "started": started}),
                    );
                    self.model.on_connected()
                }
                Msg::Failed { code, message, .. } => {
                    self.connecting = false;
                    self.event(&json!({"event": "connect_failed", "code": code}));
                    self.model.on_connect_failed(code, &message)
                }
                Msg::Control(v) => self.model.on_control(&v),
                Msg::Data { .. } => Vec::new(), // never attached
                Msg::Closed => {
                    self.link = None;
                    self.event(&json!({"event": "disconnected"}));
                    self.model.on_disconnected()
                }
            };
            self.apply(fx);
        }
    }

    fn apply(&mut self, effects: Vec<Effect>) {
        let mut changed = false;
        for e in effects {
            match e {
                Effect::Send(m) => {
                    if let Some(l) = &self.link {
                        l.send(&m);
                    }
                }
                Effect::Changed => changed = true,
                Effect::Reconnect(d) => {
                    if self.connecting {
                        continue;
                    }
                    if d.is_zero() {
                        // SAFETY: our own window's timer.
                        unsafe { KillTimer(self.icon, TIMER_RECONNECT) };
                        self.connect(false);
                    } else {
                        let ms = u32::try_from(d.as_millis()).unwrap_or(u32::MAX).max(1);
                        // SAFETY: a one-shot use of our own window's timer.
                        unsafe { SetTimer(self.icon, TIMER_RECONNECT, ms, None) };
                    }
                }
                Effect::Connect { start } => {
                    // SAFETY: our own window's timer.
                    unsafe { KillTimer(self.icon, TIMER_RECONNECT) };
                    self.connect(start);
                }
                Effect::Open(e) => {
                    // The manager closes after it handed the Terminal on (unless pinned).
                    self.after.push(After::Open(e, !self.prefs.manager_pinned));
                }
                Effect::Notice(n) => {
                    self.event(&json!({"event": "notice", "text": n}));
                    if let Some(ui) = &mut self.ui {
                        ui.notice = Some(n);
                    } else {
                        self.notice = Some(n);
                    }
                    changed = true;
                }
                Effect::Renamed(r) => {
                    self.event(
                        &json!({"event": "renamed", "ok": r.is_ok(), "error": r.as_ref().err()}),
                    );
                    if let (Err(e), Some(ui)) = (r, &mut self.ui) {
                        ui.notice = Some(format!("The name was not changed: {e}"));
                    }
                    changed = true;
                }
            }
        }
        if changed {
            self.sync_ui();
            self.refresh_icon(false);
            self.event(&json!({"event": "state", "conn": self.model.conn.name(),
                "terminals": self.model.entries().len(), "summary": self.model.summary()}));
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The Terminal manager.

/// Creates one child control of the manager.
fn child(parent: HWND, class: &str, text: &str, style: u32, id: i32) -> HWND {
    let c = wide(class);
    let t = wide(text);
    // SAFETY: a child window of our own manager window; destroyed with it.
    unsafe {
        CreateWindowExW(
            0,
            c.as_ptr(),
            t.as_ptr(),
            WS_CHILD | style,
            0,
            0,
            1,
            1,
            parent,
            id as usize as _,
            GetModuleHandleW(null()),
            null(),
        )
    }
}

/// `WM_SETREDRAW`.
const WM_SETREDRAW: u32 = 0x000B;
/// `ODS_DISABLED`.
const ODS_DISABLED: u32 = 0x0004;
/// `EN_SETFOCUS`, `LBN_SETFOCUS` and `BN_SETFOCUS` (with `BS_NOTIFY`): the system focus moved.
const EN_SETFOCUS: u32 = 0x0100;
const LBN_SETFOCUS: u32 = 4;
const BS_NOTIFY: u32 = 0x4000;
/// `LB_ITEMFROMPOINT`.
const LB_ITEMFROMPOINT: u32 = 0x01A9;
/// `WA_INACTIVE`.
const WA_INACTIVE: u32 = 0;
/// `WM_PRINTCLIENT`: draw into the given DC (`PrintWindow`, the test hook's picture).
const WM_PRINTCLIENT: u32 = 0x0318;

impl Float {
    fn toggle_manager(&mut self, anchor: Option<(i32, i32)>) {
        if self.ui.is_some() {
            self.close_manager();
        } else {
            self.open_manager(anchor);
        }
    }

    fn anchor_rect(&self, anchor: Option<(i32, i32)>) -> Rect {
        let at = anchor.or(if self.prefs.icon {
            None
        } else {
            self.tray_anchor
        });
        match at {
            Some((x, y)) => Rect {
                left: x,
                top: y,
                right: x + 1,
                bottom: y + 1,
            },
            None => self.icon_rect(),
        }
    }

    fn manager_rect(&self, anchor: Option<(i32, i32)>) -> Option<(Rect, u32)> {
        place::manager_rect(&self.anchor_rect(anchor), &self.monitors())
    }

    /// Opens the manager beside the icon (or the tray's click), refreshing the list.
    fn open_manager(&mut self, anchor: Option<(i32, i32)>) {
        if let Some(ui) = &self.ui {
            if !self.opts.test {
                self.after.push(After::Foreground(ui.hwnd));
            }
            return;
        }
        let Some((rect, dpi)) = self.manager_rect(anchor) else {
            return;
        };
        if !self.allowed(&rect) {
            self.event(&json!({"event": "error", "message": "a test window may not be placed on screen", "rect": rect_json(&rect)}));
            return;
        }
        self.matcher.reload();
        let ex = WS_EX_TOOLWINDOW
            | if self.prefs.on_top { WS_EX_TOPMOST } else { 0 }
            | if self.opts.test { WS_EX_NOACTIVATE } else { 0 };
        let class = wide(MANAGER_CLASS);
        let title = wide("Subak — Terminals");
        // SAFETY: a popup window of our own class; destroyed when the manager closes.
        let hwnd = unsafe {
            CreateWindowExW(
                ex,
                class.as_ptr(),
                title.as_ptr(),
                WS_POPUP | WS_CLIPCHILDREN,
                rect.left,
                rect.top,
                rect.width(),
                rect.height(),
                null_mut(),
                null_mut(),
                GetModuleHandleW(null()),
                null::<CREATESTRUCTW>().cast(),
            )
        };
        if hwnd.is_null() {
            self.event(&json!({"event": "error", "message": "cannot create the manager"}));
            return;
        }
        let stat = WS_VISIBLE | SS_TEXT;
        let button = WS_VISIBLE | WS_TABSTOP | BS_OWNERDRAW as u32 | BS_NOTIFY;
        // Creation order is the z-order, which the system's accessibility proxies use to name
        // a control by the label before it: "Search" names the field, "Terminals" the list.
        let ctls = vec![
            (ID_SUMMARY, child(hwnd, "STATIC", "", stat, ID_SUMMARY)),
            (
                ID_PIN,
                child(hwnd, "BUTTON", "Pin (keep open)", button, ID_PIN),
            ),
            (
                ID_CLOSE,
                child(hwnd, "BUTTON", "Close manager", button, ID_CLOSE),
            ),
            (
                ID_SEARCH_LABEL,
                child(hwnd, "STATIC", "Search", stat, ID_SEARCH_LABEL),
            ),
            (
                ID_SEARCH,
                child(
                    hwnd,
                    "EDIT",
                    "",
                    WS_VISIBLE | WS_TABSTOP | ES_AUTOHSCROLL as u32,
                    ID_SEARCH,
                ),
            ),
            (ID_TITLE, child(hwnd, "STATIC", "Terminals", stat, ID_TITLE)),
            (
                ID_LIST,
                child(
                    hwnd,
                    "LISTBOX",
                    "Terminals",
                    WS_VISIBLE
                        | WS_TABSTOP
                        | WS_VSCROLL
                        | 0x0400_0000 // WS_CLIPSIBLINGS
                        | (LBS_OWNERDRAWFIXED | LBS_HASSTRINGS | LBS_NOTIFY | LBS_NOINTEGRALHEIGHT)
                            as u32,
                    ID_LIST,
                ),
            ),
            (ID_EMPTY, child(hwnd, "STATIC", "", SS_TEXT, ID_EMPTY)),
            (ID_STATUS, child(hwnd, "STATIC", "", stat, ID_STATUS)),
            (ID_ASK, child(hwnd, "STATIC", "", SS_TEXT, ID_ASK)),
            (
                ID_NEW,
                child(hwnd, "BUTTON", "New Terminal", button, ID_NEW),
            ),
            (ID_OPEN, child(hwnd, "BUTTON", "Open", button, ID_OPEN)),
            (
                ID_RENAME,
                child(hwnd, "BUTTON", "Rename", button, ID_RENAME),
            ),
            (
                ID_DELETE,
                child(hwnd, "BUTTON", "Delete…", button, ID_DELETE),
            ),
            (
                ID_CANCEL,
                child(
                    hwnd,
                    "BUTTON",
                    "Cancel",
                    BS_OWNERDRAW as u32 | WS_TABSTOP,
                    ID_CANCEL,
                ),
            ),
            (
                ID_END,
                child(
                    hwnd,
                    "BUTTON",
                    "End Terminal",
                    BS_OWNERDRAW as u32 | WS_TABSTOP,
                    ID_END,
                ),
            ),
        ];
        let ui = Ui {
            hwnd,
            dpi,
            paint: Paint::new(dpi, self.hc),
            ctls,
            rename: None,
            focus: ID_SEARCH,
            items: Vec::new(),
            notice: self.notice.take(),
            syncing: false,
            opened: Instant::now(),
        };
        self.ui = Some(ui);
        self.set_fonts();
        let _ = self.model.set_filter("");
        self.layout_manager();
        self.sync_ui();
        if self.opts.test {
            self.after.push(After::Show(hwnd, SW_SHOWNOACTIVATE));
        } else {
            // The user opened it: it takes the focus, the search field first.
            self.after.push(After::Show(hwnd, SW_SHOW));
            self.after.push(After::Foreground(hwnd));
            if let Some(ui) = &self.ui {
                self.after.push(After::Focus(ui.ctl(ID_SEARCH)));
            }
        }
        self.event(
            &json!({"event": "manager", "open": true, "hwnd": hwnd as usize,
            "rect": rect_json(&rect), "dpi": dpi}),
        );
        let fx = self.model.refresh();
        self.apply(fx);
    }

    /// Closes the manager: a question still open is answered Cancel (nothing ends), a rename
    /// is dropped. The Host and its Terminals are not touched.
    fn close_manager(&mut self) {
        let Some(ui) = self.ui.take() else {
            return;
        };
        if matches!(self.model.mode, Mode::Ask(_)) {
            let _ = self.model.answer(Choice::Cancel);
        }
        let _ = self.model.cancel_rename();
        let open_ms = ui.opened.elapsed().as_millis() as u64;
        self.after.push(After::CloseUi(Box::new(ui)));
        self.event(&json!({"event": "manager", "open": false, "open_ms": open_ms}));
    }

    fn place_manager(&mut self) {
        let Some((rect, dpi)) = self.manager_rect(None) else {
            return;
        };
        if !self.allowed(&rect) {
            return;
        }
        let Some(ui) = &mut self.ui else {
            return;
        };
        if ui.dpi != dpi {
            ui.dpi = dpi;
            ui.paint = Paint::new(dpi, self.hc);
        }
        let hwnd = ui.hwnd;
        self.after.push(After::Place {
            hwnd,
            rect,
            topmost: None,
        });
        self.set_fonts();
    }

    fn set_fonts(&self) {
        let Some(ui) = &self.ui else {
            return;
        };
        for (id, h) in &ui.ctls {
            let f = if *id == ID_TITLE {
                ui.paint.title
            } else {
                ui.paint.body
            };
            // SAFETY: our own controls; the font outlives them (it goes with the `Ui`).
            unsafe { SendMessageW(*h, WM_SETFONT, f as WPARAM, 1) };
        }
        if let Some(r) = ui.rename {
            // SAFETY: as above.
            unsafe { SendMessageW(r, WM_SETFONT, ui.paint.body as WPARAM, 1) };
        }
    }

    /// Places the controls for the client area at the manager's DPI (logical pixels: 8 padding,
    /// 28 controls, 6 gaps, 32 rows).
    fn layout_manager(&self) {
        let Some(ui) = &self.ui else {
            return;
        };
        let mut rc = RECT::default();
        // SAFETY: our own window's client rectangle.
        unsafe { GetClientRect(ui.hwnd, &raw mut rc) };
        let (w, h) = (rc.right, rc.bottom);
        let p = |v: f32| px(v, ui.dpi);
        let (pad, ctl, gap) = (p(8.0), p(28.0), p(6.0));
        let mv = |id: i32, x: i32, y: i32, cw: i32, ch: i32| {
            // SAFETY: moves one of our own child controls.
            unsafe { MoveWindow(ui.ctl(id), x, y, cw.max(1), ch.max(1), 1) };
        };
        let pin_w = p(56.0);
        mv(ID_CLOSE, w - pad - ctl, pad, ctl, ctl);
        mv(ID_PIN, w - pad - ctl - gap - pin_w, pad, pin_w, ctl);
        mv(ID_TITLE, pad + p(4.0), pad, p(92.0), ctl);
        let sx = pad + p(100.0);
        mv(
            ID_SUMMARY,
            sx,
            pad + p(6.0),
            w - pad - ctl - 2 * gap - pin_w - sx,
            ctl - p(6.0),
        );
        let sy = pad + ctl + gap;
        mv(
            ID_SEARCH_LABEL,
            pad + p(4.0),
            sy + p(6.0),
            p(48.0),
            ctl - p(6.0),
        );
        mv(
            ID_SEARCH,
            pad + p(56.0),
            sy + p(3.0),
            w - 2 * pad - p(60.0),
            ctl - p(6.0),
        );
        let ay = h - pad - ctl;
        let bw = (w - 2 * pad - 3 * gap) / 4;
        for (k, id) in [ID_NEW, ID_OPEN, ID_RENAME, ID_DELETE]
            .into_iter()
            .enumerate()
        {
            mv(id, pad + k as i32 * (bw + gap), ay, bw, ctl);
        }
        let end_w = p(116.0);
        let cancel_w = p(84.0);
        mv(ID_END, w - pad - end_w, ay, end_w, ctl);
        mv(
            ID_CANCEL,
            w - pad - end_w - gap - cancel_w,
            ay,
            cancel_w,
            ctl,
        );
        let st_h = p(34.0);
        let sty = ay - gap - st_h;
        mv(ID_STATUS, pad + p(4.0), sty, w - 2 * pad - p(8.0), st_h);
        mv(ID_ASK, pad + p(4.0), sty, w - 2 * pad - p(8.0), st_h);
        let ly = sy + ctl + gap;
        let lh = sty - gap - ly;
        mv(ID_LIST, pad, ly, w - 2 * pad, lh);
        mv(
            ID_EMPTY,
            pad + p(12.0),
            ly + p(12.0),
            w - 2 * pad - p(24.0),
            lh - p(24.0),
        );
        // SAFETY: sets the row height of our own owner-drawn list box.
        unsafe { SendMessageW(ui.ctl(ID_LIST), LB_SETITEMHEIGHT, 0, p(ROW) as LPARAM) };
    }

    /// The manager's controls follow the model: rows, selection, the empty sentence, the status
    /// line, the question, the rename field, which buttons work.
    fn sync_ui(&mut self) {
        let new_keys = self.matcher.keys_for(Action::ManagerNew).map(String::from);
        let hint = {
            let k = |a| self.matcher.keys_for(a).unwrap_or("—").to_string();
            format!(
                "{}: open · {}: new · {}: rename · {}: delete · {}: search",
                k(Action::ManagerOpen),
                k(Action::ManagerNew),
                k(Action::ManagerRename),
                k(Action::ManagerDelete),
                k(Action::ManagerSearch)
            )
        };
        let pinned = self.prefs.manager_pinned;
        let connected = self.model.connected();
        let summary = self.model.summary();
        let items: Vec<String> = self
            .model
            .visible()
            .iter()
            .map(|e| e.accessible())
            .collect();
        let sel = self.model.selected_index();
        let empty = self.model.empty_text(new_keys.as_deref());
        let mode = self.model.mode.clone();
        let selected = self.model.selected().cloned();
        let mut focus_to: Option<i32> = None;
        let Some(ui) = &mut self.ui else {
            return;
        };
        ui.syncing = true;
        set_text(ui.ctl(ID_SUMMARY), &summary);
        let list = ui.ctl(ID_LIST);
        // SAFETY: messages to our own list box; the strings outlive each call.
        unsafe {
            if items != ui.items {
                let top = SendMessageW(list, LB_GETTOPINDEX, 0, 0);
                SendMessageW(list, WM_SETREDRAW, 0, 0);
                SendMessageW(list, LB_RESETCONTENT, 0, 0);
                for s in &items {
                    let w = wide(s);
                    SendMessageW(list, LB_ADDSTRING, 0, w.as_ptr() as LPARAM);
                }
                if top >= 0 {
                    SendMessageW(list, LB_SETTOPINDEX, top as WPARAM, 0);
                }
                SendMessageW(list, WM_SETREDRAW, 1, 0);
                ui.items = items;
            }
            let want = sel.map_or(-1, usize::cast_signed);
            if SendMessageW(list, LB_GETCURSEL, 0, 0) != want {
                SendMessageW(list, LB_SETCURSEL, want as WPARAM, 0);
            }
            InvalidateRect(list, null(), 1);
        }
        let show = |id: i32, on: bool| {
            // SAFETY: shows or hides one of our own controls.
            unsafe { ShowWindow(ui.ctl(id), if on { SW_SHOW } else { SW_HIDE }) };
        };
        show(ID_LIST, empty.is_none());
        show(ID_EMPTY, empty.is_some());
        set_text(ui.ctl(ID_EMPTY), empty.as_deref().unwrap_or(""));
        let asking = matches!(mode, Mode::Ask(_));
        for id in [ID_NEW, ID_OPEN, ID_RENAME, ID_DELETE, ID_STATUS] {
            show(id, !asking);
        }
        for id in [ID_ASK, ID_CANCEL, ID_END] {
            show(id, asking);
        }
        let status = ui.notice.clone().unwrap_or(hint);
        set_text(ui.ctl(ID_STATUS), &status);
        let enable = |id: i32, on: bool| {
            // SAFETY: enables or disables one of our own controls.
            unsafe { EnableWindow(ui.ctl(id), i32::from(on)) };
        };
        let has_sel = selected.is_some() && connected;
        enable(ID_OPEN, has_sel);
        enable(ID_RENAME, has_sel);
        enable(ID_DELETE, has_sel);
        set_text(
            ui.ctl(ID_PIN),
            if pinned {
                "Pinned (keep open: on)"
            } else {
                "Pin (keep open: off)"
            },
        );
        if let Mode::Ask(a) = &mode {
            set_text(ui.ctl(ID_ASK), &a.text());
            set_text(ui.ctl(ID_END), a.action());
            let f = if a.focus == Choice::End {
                ID_END
            } else {
                ID_CANCEL
            };
            if ui.focus != f {
                focus_to = Some(f);
            }
        } else if matches!(ui.focus, ID_CANCEL | ID_END) {
            focus_to = Some(ID_LIST);
        }
        // The rename field over the selected row.
        match (&mode, selected.as_ref()) {
            (Mode::Rename { id, before }, Some(e)) if &e.id == id => {
                let mut r = RECT::default();
                let idx = sel.unwrap_or(0);
                // SAFETY: the item's rectangle in the list box, mapped to the manager.
                unsafe {
                    SendMessageW(list, LB_GETITEMRECT, idx, (&raw mut r) as LPARAM);
                    let mut lr = RECT::default();
                    GetWindowRect(list, &raw mut lr);
                    let mut mr = RECT::default();
                    GetWindowRect(ui.hwnd, &raw mut mr);
                    let dx = lr.left - mr.left;
                    let dy = lr.top - mr.top;
                    let inset = px(30.0, ui.dpi);
                    let (x, y, cw, ch) = (
                        dx + r.left + inset,
                        dy + r.top + px(4.0, ui.dpi),
                        (r.right - r.left) - inset - px(8.0, ui.dpi),
                        (r.bottom - r.top) - px(8.0, ui.dpi),
                    );
                    let field = match ui.rename {
                        Some(h) => h,
                        None => {
                            let h = child(
                                ui.hwnd,
                                "EDIT",
                                before,
                                WS_VISIBLE | WS_TABSTOP | 0x0400_0000 | ES_AUTOHSCROLL as u32,
                                ID_RENAME_EDIT,
                            );
                            SendMessageW(h, WM_SETFONT, ui.paint.body as WPARAM, 1);
                            SendMessageW(h, EM_SETSEL, 0, -1);
                            ui.rename = Some(h);
                            focus_to = Some(ID_RENAME_EDIT);
                            h
                        }
                    };
                    SetWindowPos(
                        field,
                        null_mut(),
                        x,
                        y,
                        cw.max(1),
                        ch.max(1),
                        SWP_NOACTIVATE,
                    );
                }
            }
            _ => {
                if let Some(h) = ui.rename.take() {
                    // SAFETY: our own child control.
                    unsafe { DestroyWindow(h) };
                    if ui.focus == ID_RENAME_EDIT {
                        focus_to = Some(ID_LIST);
                    }
                }
            }
        }
        ui.syncing = false;
        // SAFETY: repaints our own window (the owner-drawn buttons).
        unsafe {
            RedrawWindow(
                ui.hwnd,
                null(),
                null_mut(),
                RDW_INVALIDATE | RDW_ALLCHILDREN,
            )
        };
        if let Some(f) = focus_to {
            self.set_focus(f);
        }
    }

    /// Moves the keyboard focus inside the manager (the system focus too, except in the test
    /// mode, which never takes the focus).
    fn set_focus(&mut self, id: i32) {
        let test = self.opts.test;
        let Some(ui) = &mut self.ui else {
            return;
        };
        ui.focus = id;
        let h = ui.ctl(id);
        if !test && !h.is_null() {
            self.after.push(After::Focus(h));
        }
        // SAFETY: repaints our own window (focus rings).
        unsafe {
            RedrawWindow(
                ui.hwnd,
                null(),
                null_mut(),
                RDW_INVALIDATE | RDW_ALLCHILDREN,
            )
        };
        let (hwnd, focus) = (ui.hwnd, ui.focus);
        self.event(
            &json!({"event": "focus", "control": ctl_name(focus), "manager": hwnd as usize}),
        );
    }

    fn cycle_focus(&mut self, back: bool) {
        let Some(ui) = &self.ui else {
            return;
        };
        // SAFETY: visibility and enabled state of our own controls.
        let usable = |id: i32| unsafe {
            let h = ui.ctl(id);
            !h.is_null()
                && IsWindowVisible(h) != 0
                && windows_sys::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(h) != 0
        };
        let order: Vec<i32> = FOCUS_ORDER.into_iter().filter(|i| usable(*i)).collect();
        if order.is_empty() {
            return;
        }
        let cur = order.iter().position(|i| *i == ui.focus).unwrap_or(0);
        let n = order.len();
        let next = if back {
            (cur + n - 1) % n
        } else {
            (cur + 1) % n
        };
        self.set_focus(order[next]);
    }

    fn run(&mut self, a: Action) {
        self.event(&json!({"event": "action", "action": a.id()}));
        let fx = match a {
            Action::ManagerOpen => self.model.open_selected(),
            Action::ManagerNew => self.model.create(),
            Action::ManagerRename => self.model.start_rename(),
            Action::ManagerDelete => self.model.ask_delete(),
            Action::ManagerSearch => {
                self.set_focus(ID_SEARCH);
                if let Some(ui) = &self.ui {
                    // SAFETY: selects the search field's text.
                    unsafe { SendMessageW(ui.ctl(ID_SEARCH), EM_SETSEL, 0, -1) };
                }
                Vec::new()
            }
            Action::ManagerPin => {
                self.prefs.manager_pinned = !self.prefs.manager_pinned;
                self.save_prefs();
                vec![Effect::Changed]
            }
            Action::ManagerClose => {
                self.close_manager();
                Vec::new()
            }
            _ => Vec::new(),
        };
        self.apply(fx);
    }

    fn button(&mut self, id: i32) {
        match id {
            ID_NEW => self.run(Action::ManagerNew),
            ID_OPEN => self.run(Action::ManagerOpen),
            ID_RENAME => self.run(Action::ManagerRename),
            ID_DELETE => self.run(Action::ManagerDelete),
            ID_PIN => self.run(Action::ManagerPin),
            ID_CLOSE => self.run(Action::ManagerClose),
            ID_CANCEL | ID_END => {
                let c = if id == ID_END {
                    Choice::End
                } else {
                    Choice::Cancel
                };
                let fx = self.model.answer(c);
                self.apply(fx);
            }
            _ => {}
        }
    }

    /// One key press in the manager (`id`: the control it is for). Returns whether it was
    /// handled (else the control gets it: typing, editing, the list's own keys).
    fn manager_key(&mut self, id: i32, p: &Press, m: Mods) -> bool {
        let Some(ui) = &mut self.ui else {
            return false;
        };
        if id != 0 && ui.focus != id {
            ui.focus = id;
        }
        let code = p.vk;
        let plain = !m.ctrl && !m.alt;
        // 1. The question owns every key (Cancel is the default; nothing else reacts).
        if let Mode::Ask(a) = self.model.mode.clone() {
            let fx = match code {
                vk::ESCAPE => self.model.answer(Choice::Cancel),
                vk::RETURN | vk::SPACE => {
                    let c = if self.ui.as_ref().is_some_and(|u| u.focus == ID_END) {
                        Choice::End
                    } else {
                        a.focus
                    };
                    self.model.answer(c)
                }
                vk::TAB | vk::LEFT | vk::RIGHT | vk::UP | vk::DOWN => {
                    let next = if a.focus == Choice::Cancel {
                        Choice::End
                    } else {
                        Choice::Cancel
                    };
                    self.model.ask_focus(next)
                }
                _ => Vec::new(),
            };
            self.apply(fx);
            return true;
        }
        // 2. The rename field: Enter saves, Esc cancels, everything else edits.
        if matches!(self.model.mode, Mode::Rename { .. }) && id == ID_RENAME_EDIT {
            let fx = match code {
                vk::RETURN => {
                    let text = self
                        .ui
                        .as_ref()
                        .and_then(|u| u.rename)
                        .map(text_of)
                        .unwrap_or_default();
                    self.model.commit_rename(text.trim())
                }
                vk::ESCAPE => self.model.cancel_rename(),
                vk::TAB => Vec::new(),
                _ => return false,
            };
            self.apply(fx);
            return true;
        }
        // 3. Navigation.
        if code == vk::TAB && !m.ctrl && !m.alt {
            self.cycle_focus(m.shift);
            return true;
        }
        let in_field = id == ID_SEARCH;
        if plain && (in_field || id == ID_LIST) {
            let rows = self.page_rows();
            let delta = match code {
                vk::UP => Some(-1),
                vk::DOWN => Some(1),
                vk::PRIOR => Some(-rows),
                vk::NEXT => Some(rows),
                vk::HOME if id == ID_LIST => Some(isize::MIN),
                vk::END if id == ID_LIST => Some(isize::MAX),
                _ => None,
            };
            if let Some(d) = delta {
                let fx = self.model.move_selection(d);
                self.apply(fx);
                return true;
            }
        }
        // 4. The search field first: its editing keys and typing.
        if in_field {
            let search = self.ui.as_ref().map_or(null_mut(), |u| u.ctl(ID_SEARCH));
            if code == vk::ESCAPE && plain && !text_of(search).is_empty() {
                set_text(search, "");
                let fx = self.model.set_filter("");
                self.apply(fx);
                return true;
            }
            if matches!(
                code,
                vk::BACK | vk::DELETE | vk::LEFT | vk::RIGHT | vk::HOME | vk::END | vk::INSERT
            ) {
                return false;
            }
            if m.ctrl && !m.alt && matches!(code, 0x41 | 0x43 | 0x56 | 0x58 | 0x59 | 0x5A) {
                return false; // Ctrl+A/C/V/X/Y/Z
            }
            if plain
                && p.ch
                    .or_else(|| produced_char(code, p.scan, m.shift))
                    .is_some_and(|c| !c.is_control())
            {
                return false;
            }
        }
        // 5. A focused button: Enter and Space press it.
        if plain && matches!(code, vk::RETURN | vk::SPACE) && (ID_NEW..=ID_CLOSE).contains(&id) {
            self.button(id);
            return true;
        }
        // 6. The manager's shortcuts (context `manager`).
        let mut press = *p;
        if press.ch.is_none() && self.matcher.wants_chars() {
            press.ch = produced_char(code, p.scan, m.shift);
        }
        match self.matcher.press(&press) {
            Outcome::Fire(a) => {
                self.run(a);
                true
            }
            Outcome::Ignored => false,
            Outcome::Pending | Outcome::Cancelled | Outcome::Swallowed | Outcome::Hold(_) => true,
        }
    }

    /// A character in the manager: typing in the list goes to the search field.
    fn manager_char(&mut self, id: i32, unit: u32) -> bool {
        if matches!(self.model.mode, Mode::Ask(_)) {
            return true;
        }
        let Some(c) = char::from_u32(unit).filter(|c| !c.is_control()) else {
            return false;
        };
        if id != ID_LIST {
            return false;
        }
        let Some(ui) = &self.ui else {
            return false;
        };
        let search = ui.ctl(ID_SEARCH);
        let text = format!("{}{c}", text_of(search));
        set_text(search, &text);
        // SAFETY: puts the caret at the end of our own field.
        unsafe { SendMessageW(search, EM_SETSEL, text.len(), text.len() as LPARAM) };
        let fx = self.model.set_filter(&text);
        self.apply(fx);
        self.set_focus(ID_SEARCH);
        true
    }

    /// Rows per page of the list (PageUp/PageDown).
    fn page_rows(&self) -> isize {
        let Some(ui) = &self.ui else {
            return 1;
        };
        let mut r = RECT::default();
        // SAFETY: our own list box's client rectangle.
        unsafe { GetClientRect(ui.ctl(ID_LIST), &raw mut r) };
        ((r.bottom / px(ROW, ui.dpi).max(1)) as isize).max(1)
    }

    fn row_menu(&mut self, at: Option<(i32, i32)>) {
        let Some(ui) = &self.ui else {
            return;
        };
        let k = |a| {
            self.matcher
                .keys_for(a)
                .map_or_else(String::new, |k| format!("\t{k}"))
        };
        let on = if self.model.selected().is_some() && self.model.connected() {
            0
        } else {
            MF_GRAYED
        };
        let items = vec![
            (CMD_ROW_OPEN, format!("Open{}", k(Action::ManagerOpen)), on),
            (
                CMD_ROW_RENAME,
                format!("Rename…{}", k(Action::ManagerRename)),
                on,
            ),
            (
                CMD_ROW_DELETE,
                format!("Delete…{}", k(Action::ManagerDelete)),
                on,
            ),
            (0, String::new(), 0),
            (
                CMD_ROW_NEW,
                format!("New Terminal{}", k(Action::ManagerNew)),
                0,
            ),
        ];
        let at = at.unwrap_or_else(|| {
            let mut r = RECT::default();
            // SAFETY: our own list box's screen rectangle.
            unsafe { GetWindowRect(ui.ctl(ID_LIST), &raw mut r) };
            let row = px(ROW, ui.dpi);
            let i = self.model.selected_index().unwrap_or(0) as i32;
            (r.left + row, r.top + (i + 1) * row)
        });
        let owner = ui.hwnd;
        self.menu(owner, items, at);
    }

    /// Draws a list row or a button (`WM_DRAWITEM`).
    fn draw_item(&self, d: &DRAWITEMSTRUCT) {
        let Some(ui) = &self.ui else {
            return;
        };
        let c = Paint::colors(ui.paint.hc);
        let hdc = d.hDC;
        let r = d.rcItem;
        let p = |v: f32| px(v, ui.dpi);
        let draw = |text: &str, rc: RECT, font: HFONT, color: Rgb, flags: u32| {
            let w: Vec<u16> = text.encode_utf16().collect();
            let mut rc = rc;
            // SAFETY: draws into the DC Windows passed with WM_DRAWITEM.
            unsafe {
                SelectObject(hdc, font as HGDIOBJ);
                SetTextColor(hdc, colorref(color));
                DrawTextW(hdc, w.as_ptr(), w.len() as i32, &raw mut rc, flags);
            }
        };
        let one = DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_END_ELLIPSIS;
        // SAFETY: drawing into the DC Windows passed with WM_DRAWITEM.
        unsafe { SetBkMode(hdc, TRANSPARENT as i32) };
        if d.CtlType == ODT_LISTBOX {
            let visible = self.model.visible();
            let Some(e) = usize::try_from(d.itemID).ok().and_then(|i| visible.get(i)) else {
                // SAFETY: as above.
                unsafe { FillRect(hdc, &raw const r, ui.paint.bg) };
                return;
            };
            let selected = d.itemState & ODS_SELECTED != 0;
            let focused =
                d.itemState & ODS_FOCUS != 0 || (self.opts.test && ui.focus == ID_LIST && selected);
            // SAFETY: as above.
            unsafe {
                FillRect(
                    hdc,
                    &raw const r,
                    if selected {
                        ui.paint.selected
                    } else {
                        ui.paint.bg
                    },
                );
            }
            let st = e.status();
            let text = if selected { c.selected_text } else { c.text };
            let glyph_color = if ui.paint.hc { text } else { st.color() };
            let mut g = r;
            g.left += p(10.0);
            g.right = g.left + p(16.0);
            draw(
                &st.glyph().to_string(),
                g,
                ui.paint.symbol,
                glyph_color,
                one | DT_CENTER,
            );
            let width = r.right - r.left;
            let mut n = r;
            n.left += p(32.0);
            n.right = r.left + width * 45 / 100;
            draw(&e.name, n, ui.paint.body, text, one | DT_LEFT);
            let mut dt = r;
            dt.left = n.right + p(8.0);
            dt.right -= p(10.0);
            draw(
                &e.detail(),
                dt,
                ui.paint.body,
                if selected { text } else { c.muted },
                one | DT_RIGHT,
            );
            if focused {
                self.focus_ring(hdc, r, c.focus, 1);
            }
            return;
        }
        if d.CtlType != ODT_BUTTON {
            return;
        }
        let id = d.CtlID as i32;
        let disabled = d.itemState & ODS_DISABLED != 0;
        let focused = d.itemState & ODS_FOCUS != 0 || (self.opts.test && ui.focus == id);
        let pinned = self.prefs.manager_pinned;
        let (fill, fg, edge) = match id {
            ID_OPEN if !disabled => (c.accent, c.on_accent, c.accent),
            ID_PIN if pinned => (c.accent, c.on_accent, c.accent),
            ID_DELETE | ID_END => (c.bg, c.danger, c.danger),
            _ => (c.field, c.text, c.edge),
        };
        let fg = if disabled { c.muted } else { fg };
        // SAFETY: drawing into the DC Windows passed with WM_DRAWITEM; the brush and pen are
        // deselected and deleted before returning.
        unsafe {
            FillRect(hdc, &raw const r, ui.paint.bg);
            let brush = CreateSolidBrush(colorref(fill));
            let pen = CreatePen(PS_SOLID, 1, colorref(edge));
            let ob = SelectObject(hdc, brush as HGDIOBJ);
            let op = SelectObject(hdc, pen as HGDIOBJ);
            let rad = p(8.0);
            RoundRect(hdc, r.left, r.top, r.right, r.bottom, rad, rad);
            SelectObject(hdc, ob);
            SelectObject(hdc, op);
            DeleteObject(brush as HGDIOBJ);
            DeleteObject(pen as HGDIOBJ);
        }
        let label = match id {
            ID_CLOSE => "×".to_string(),
            ID_PIN => if pinned { "Pinned" } else { "Pin" }.to_string(),
            _ => text_of(d.hwndItem),
        };
        let font = if id == ID_CLOSE {
            ui.paint.title
        } else {
            ui.paint.body
        };
        draw(&label, r, font, fg, one | DT_CENTER);
        if focused {
            self.focus_ring(hdc, r, c.focus, 2);
        }
    }

    /// A focus ring `width` device pixels wide inside `r` (DESIGN_SYSTEM: visible focus).
    fn focus_ring(&self, hdc: HDC, r: RECT, color: Rgb, width: i32) {
        // SAFETY: a temporary brush, deleted below; frames inside the item's rectangle.
        unsafe {
            let b = CreateSolidBrush(colorref(color));
            let mut rc = r;
            for _ in 0..width.max(1) {
                FrameRect(hdc, &raw const rc, b);
                rc.left += 1;
                rc.top += 1;
                rc.right -= 1;
                rc.bottom -= 1;
            }
            DeleteObject(b as HGDIOBJ);
        }
    }

    fn paint_manager(&self, hwnd: HWND) {
        let mut ps = PAINTSTRUCT::default();
        // SAFETY: a paint cycle of our own window.
        unsafe {
            let hdc = BeginPaint(hwnd, &raw mut ps);
            self.paint_manager_to(hwnd, hdc);
            EndPaint(hwnd, &raw const ps);
        }
    }

    /// The manager's background and its 1 px border (a paint, or `WM_PRINTCLIENT`).
    fn paint_manager_to(&self, hwnd: HWND, hdc: HDC) {
        let Some(ui) = &self.ui else {
            return;
        };
        // SAFETY: drawing into a DC Windows passed for our own window; the brush is deleted.
        unsafe {
            let mut rc = RECT::default();
            GetClientRect(hwnd, &raw mut rc);
            FillRect(hdc, &raw const rc, ui.paint.bg);
            FrameRect(hdc, &raw const rc, ui.paint.border);
            // The search field: a raised box with a boundary that has 3:1 contrast; the focus
            // colour while it has the keyboard.
            let c = Paint::colors(ui.paint.hc);
            let mut sr = RECT::default();
            GetWindowRect(ui.ctl(ID_SEARCH), &raw mut sr);
            let mut mr = RECT::default();
            GetWindowRect(hwnd, &raw mut mr);
            let pad = px(3.0, ui.dpi);
            let field = RECT {
                left: sr.left - mr.left - px(6.0, ui.dpi),
                top: sr.top - mr.top - pad,
                right: sr.right - mr.left + px(2.0, ui.dpi),
                bottom: sr.bottom - mr.top + pad,
            };
            FillRect(hdc, &raw const field, ui.paint.field);
            let edge = CreateSolidBrush(colorref(if ui.focus == ID_SEARCH {
                c.focus
            } else {
                c.edge
            }));
            FrameRect(hdc, &raw const field, edge);
            DeleteObject(edge as HGDIOBJ);
        }
    }

    /// The colours of a static, edit or list control (`WM_CTLCOLOR*`).
    fn ctl_color(&self, msg: u32, hdc: HDC, ctl: HWND) -> Option<LRESULT> {
        let ui = self.ui.as_ref()?;
        let c = Paint::colors(ui.paint.hc);
        let id = ui.id_of(ctl).unwrap_or(0);
        let (fg, bg, brush) = match msg {
            WM_CTLCOLOREDIT => (c.text, c.field, ui.paint.field),
            WM_CTLCOLORLISTBOX => (c.text, c.bg, ui.paint.bg),
            _ => {
                let fg = match id {
                    ID_TITLE => c.text,
                    ID_ASK => c.danger,
                    ID_STATUS if ui.notice.is_some() => c.text,
                    _ => c.muted,
                };
                (fg, c.bg, ui.paint.bg)
            }
        };
        // SAFETY: colours for the DC Windows passed with WM_CTLCOLOR*.
        unsafe {
            SetTextColor(hdc, colorref(fg));
            SetBkColor(hdc, colorref(bg));
        }
        Some(brush as LRESULT)
    }

    fn on_command(&mut self, wp: WPARAM, ctl: HWND) {
        let id = (wp & 0xFFFF) as i32;
        let code = ((wp >> 16) & 0xFFFF) as u32;
        let Some(ui) = &mut self.ui else {
            return;
        };
        if ui.syncing {
            return;
        }
        match (id, code) {
            (ID_SEARCH, EN_CHANGE) => {
                let text = text_of(ctl);
                let fx = self.model.set_filter(&text);
                self.apply(fx);
            }
            (ID_SEARCH, EN_SETFOCUS) | (ID_RENAME_EDIT, EN_SETFOCUS) => ui.focus = id,
            (ID_LIST, LBN_SETFOCUS) => ui.focus = id,
            (ID_LIST, LBN_SELCHANGE) => {
                // SAFETY: reads our own list box's selection.
                let i = unsafe { SendMessageW(ctl, LB_GETCURSEL, 0, 0) };
                if let Ok(i) = usize::try_from(i) {
                    let fx = self.model.select_index(i);
                    self.apply(fx);
                }
            }
            (ID_LIST, LBN_DBLCLK) => self.run(Action::ManagerOpen),
            (_, BN_CLICKED) => self.button(id),
            (_, 6) => ui.focus = id, // BN_SETFOCUS
            _ => {}
        }
    }

    // -----------------------------------------------------------------------------------------
    // Test hooks.

    fn controls_json(&self) -> Value {
        let Some(ui) = &self.ui else {
            return Value::Null;
        };
        let mut v: Vec<Value> = Vec::new();
        let mut all: Vec<(i32, HWND)> = ui.ctls.clone();
        if let Some(r) = ui.rename {
            all.push((ID_RENAME_EDIT, r));
        }
        for (id, h) in all {
            // SAFETY: state queries on our own controls.
            let (visible, enabled) = unsafe {
                (
                    IsWindowVisible(h) != 0,
                    windows_sys::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(h) != 0,
                )
            };
            v.push(
                json!({"control": ctl_name(id), "class": class_of(h), "text": text_of(h),
                "visible": visible, "enabled": enabled, "hwnd": h as usize}),
            );
        }
        Value::Array(v)
    }

    fn list_strings(&self) -> Vec<String> {
        let Some(ui) = &self.ui else {
            return Vec::new();
        };
        let list = ui.ctl(ID_LIST);
        // SAFETY: reads our own list box's strings into buffers of their reported lengths.
        unsafe {
            let n = SendMessageW(list, LB_GETCOUNT, 0, 0).max(0) as usize;
            (0..n)
                .map(|i| {
                    let len = SendMessageW(list, LB_GETTEXTLEN, i, 0);
                    if len == LB_ERR as isize || len < 0 {
                        return String::new();
                    }
                    let mut buf = vec![0u16; len as usize + 1];
                    let got = SendMessageW(list, LB_GETTEXT, i, buf.as_mut_ptr() as LPARAM);
                    String::from_utf16_lossy(&buf[..usize::try_from(got).unwrap_or(0)])
                })
                .collect()
        }
    }

    fn dump(&self) -> Value {
        // SAFETY: plain queries about our own windows and process.
        let (ex, visible, gdi, user) = unsafe {
            (
                GetWindowLongPtrW(self.icon, GWL_EXSTYLE) as u32,
                IsWindowVisible(self.icon) != 0,
                GetGuiResources(GetCurrentProcess(), 0),
                GetGuiResources(GetCurrentProcess(), 1),
            )
        };
        let state = self.model.icon_state();
        let ui = self.ui.as_ref().map(|u| {
            let mut r = RECT::default();
            // SAFETY: our own window's rectangle and ex-style.
            let uex = unsafe {
                GetWindowRect(u.hwnd, &raw mut r);
                GetWindowLongPtrW(u.hwnd, GWL_EXSTYLE) as u32
            };
            json!({
                "hwnd": u.hwnd as usize, "rect": rect_json(&rect_of(&r)), "dpi": u.dpi,
                "title": text_of(u.hwnd),
                "topmost": uex & WS_EX_TOPMOST != 0, "noactivate": uex & WS_EX_NOACTIVATE != 0,
                "toolwindow": uex & WS_EX_TOOLWINDOW != 0,
                "focus": ctl_name(u.focus), "notice": u.notice,
                "controls": self.controls_json(), "items": self.list_strings(),
                "search": text_of(u.ctl(ID_SEARCH)),
                "rename_text": u.rename.map(text_of),
            })
        });
        let mode = match &self.model.mode {
            Mode::Browse => json!({"mode": "browse"}),
            Mode::Rename { id, before } => json!({"mode": "rename", "id": id, "before": before}),
            Mode::Ask(a) => json!({"mode": "ask", "id": a.id, "name": a.name, "text": a.text(),
                "action": a.action(), "focus": if a.focus == Choice::End { "end" } else { "cancel" }}),
        };
        json!({
            "event": "dump",
            "endpoint": self.endpoint,
            "conn": self.model.conn.name(),
            "summary": self.model.summary(),
            "empty": self.model.empty_text(self.matcher.keys_for(Action::ManagerNew)),
            "entries": self.model.entries().iter().map(|e| json!({"id": e.id, "name": e.name,
                "state": e.state, "detail": e.detail(), "busy": e.busy, "clients": e.clients}))
                .collect::<Vec<_>>(),
            "visible": self.model.visible().iter().map(|e| e.name.clone()).collect::<Vec<_>>(),
            "selected": self.model.selected().map(|e| e.id.clone()),
            "filter": self.model.filter(),
            "mode": mode,
            "icon": {
                "hwnd": self.icon as usize, "visible": visible, "pos": [self.pos.0, self.pos.1],
                "size": self.size(), "dpi": self.dpi, "topmost": ex & WS_EX_TOPMOST != 0,
                "noactivate": ex & WS_EX_NOACTIVATE != 0, "toolwindow": ex & WS_EX_TOOLWINDOW != 0,
                "layered": ex & WS_EX_LAYERED != 0, "name": text_of(self.icon),
                "badge": art::badge(&state).name(), "renders": self.renders,
                "picture": self.picture.as_ref().map(|p| format!("{:016x}", p.hash())),
            },
            "tray": {
                "shown": self.tray.shown(),
                "calls": self.tray.calls().iter().map(|c| match c {
                    TrayCall::Add { tip, picture } => json!({"call": "add", "tip": tip, "picture": format!("{picture:016x}")}),
                    TrayCall::Modify { tip, picture } => json!({"call": "modify", "tip": tip, "picture": format!("{picture:016x}")}),
                    TrayCall::Delete => json!({"call": "delete"}),
                }).collect::<Vec<_>>(),
            },
            "manager": ui,
            "prefs": self.prefs.to_json(),
            "monitors": self.monitors().iter().map(|m| json!([m.work.left, m.work.top,
                m.work.right, m.work.bottom, m.dpi])).collect::<Vec<_>>(),
            "connecting": self.connecting,
            "gdi_objects": gdi, "user_objects": user,
            "shortcuts": self.matcher.bindings().into_iter()
                .map(|(a, k, s)| json!({"action": a, "keys": k, "scope": s})).collect::<Vec<_>>(),
            "opened": self.opened,
        })
    }

    fn test_command(&mut self, cmd: &Value) {
        let u32_of = |k: &str, d: u32| {
            cmd[k]
                .as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(d)
        };
        match cmd["cmd"].as_str().unwrap_or("") {
            "dump" => {}
            "monitors" => {
                let list: Option<Vec<Monitor>> = cmd["list"].as_array().map(|a| {
                    a.iter()
                        .filter_map(|m| {
                            let n = |i: usize| m[i].as_i64().and_then(|v| i32::try_from(v).ok());
                            Some(Monitor {
                                work: Rect {
                                    left: n(0)?,
                                    top: n(1)?,
                                    right: n(2)?,
                                    bottom: n(3)?,
                                },
                                dpi: u32::try_from(n(4)?).ok()?,
                            })
                        })
                        .collect()
                });
                match list.filter(|l| !l.is_empty() && l.iter().all(|m| self.allowed(&m.work))) {
                    Some(l) => self.test_monitors = Some(l),
                    None => {
                        self.event(&json!({"event": "error", "message": "monitors must lie off every real monitor"}));
                        return;
                    }
                }
            }
            "dpi" => self.on_dpi(u32_of("dpi", 96), None),
            "click" => self.toggle_manager(None),
            "manager" => {
                if cmd["open"] == true {
                    self.open_manager(None);
                } else {
                    self.close_manager();
                }
            }
            "key" => {
                let id = cmd["to"].as_str().and_then(ctl_of).unwrap_or(0);
                let m = Mods {
                    shift: cmd["shift"] == true,
                    ctrl: cmd["ctrl"] == true,
                    alt: cmd["alt"] == true,
                };
                let p = Press {
                    vk: u32_of("vk", 0),
                    scan: u32_of("scan", 0),
                    extended: false,
                    mods: keys::contract_mods(m),
                    ch: cmd["char"].as_str().and_then(|c| c.chars().next()),
                    repeat: cmd["repeat"] == true,
                };
                if !self.manager_key(id, &p, m)
                    && let Some(c) = p.ch
                {
                    self.manager_char(id, c as u32);
                }
            }
            "deactivate" => {
                if !self.prefs.manager_pinned {
                    self.close_manager();
                }
            }
            "tray" => {
                let x = cmd["x"].as_i64().unwrap_or(0) as u16 as usize;
                let y = cmd["y"].as_i64().unwrap_or(0) as u16 as usize;
                self.on_tray(x | (y << 16), u32_of("code", NIN_SELECT) as LPARAM);
            }
            // Without an id: the menu is reported (its own event, no dump follows).
            "menu" => match cmd["id"].as_u64() {
                Some(id) => self.menu_command(id as u32),
                None => {
                    self.icon_menu(self.pos);
                    return;
                }
            },
            "row_menu" => match cmd["id"].as_u64() {
                Some(id) => self.menu_command(id as u32),
                None => {
                    self.row_menu(None);
                    return;
                }
            },
            "taskbar_created" => {
                self.tray.delete();
                self.refresh_icon(true);
            }
            "shot" => {
                let path = cmd["path"].as_str().unwrap_or("").to_string();
                match cmd["what"].as_str() {
                    // The manager draws itself into memory once the state is free (its
                    // controls ask this window for their colours and owner-drawn parts).
                    Some("manager") => {
                        let hwnd = self.ui.as_ref().map_or(null_mut(), |u| u.hwnd);
                        self.after.push(After::Shot { hwnd, path });
                    }
                    _ => {
                        let r = self
                            .picture
                            .as_ref()
                            .map(|p| (p.size, p.size, p.pixels.clone()));
                        shot_event(self, &path, r.as_ref());
                    }
                }
                return;
            }
            "close" => {
                self.after.push(After::Quit);
                return;
            }
            other => {
                self.event(&json!({"event": "error", "message": format!("unknown test command `{other}`")}));
                return;
            }
        }
        self.event(&self.dump());
    }

    /// A Subak request (`COPYDATA_OPEN`): another `subak-gui --float`/`--manager` of this
    /// endpoint, forwarding to this one.
    fn open_request(&mut self, req: &Value) -> LRESULT {
        if req["endpoint"].as_str() != Some(self.endpoint.as_str()) {
            return 0;
        }
        match req["op"].as_str() {
            Some("probe") => 1,
            Some("show") | Some("manager") => {
                let (icon, tray) = (req["icon"] == true, req["tray"] == true);
                if icon || tray {
                    self.prefs.icon = icon;
                    self.prefs.tray = tray;
                    self.save_prefs();
                }
                self.event(&json!({"event": "forwarded", "op": req["op"]}));
                self.apply_presence();
                if req["op"] == "manager" {
                    self.open_manager(None);
                }
                1
            }
            _ => 0,
        }
    }
}

/// The manager's own content (`PrintWindow` into memory; never a capture of the screen).
fn shot_window(hwnd: HWND) -> Option<(u32, u32, Vec<u32>)> {
    let mut r = RECT::default();
    // SAFETY: our own window's rectangle; a DIB section of that size receives its content and
    // is freed before returning.
    unsafe {
        GetWindowRect(hwnd, &raw mut r);
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        if w <= 0 || h <= 0 {
            return None;
        }
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..BITMAPINFOHEADER::default()
            },
            ..BITMAPINFO::default()
        };
        let screen = GetDC(null_mut());
        let mem = CreateCompatibleDC(screen);
        let mut bits: *mut core::ffi::c_void = null_mut();
        let dib = CreateDIBSection(
            mem,
            &raw const bmi,
            DIB_RGB_COLORS,
            &raw mut bits,
            null_mut(),
            0,
        );
        ReleaseDC(null_mut(), screen);
        if dib.is_null() || bits.is_null() {
            DeleteDC(mem);
            return None;
        }
        let old = SelectObject(mem, dib as HGDIOBJ);
        PrintWindow(hwnd, mem, 0);
        let px = std::slice::from_raw_parts(bits.cast::<u32>(), (w * h) as usize)
            .iter()
            .map(|p| p | 0xFF00_0000)
            .collect();
        SelectObject(mem, old);
        DeleteObject(dib as HGDIOBJ);
        DeleteDC(mem);
        Some((w as u32, h as u32, px))
    }
}

/// Writes a test picture and reports it.
fn shot_event(f: &Float, path: &str, r: Option<&(u32, u32, Vec<u32>)>) {
    let ok = r.is_some_and(|(w, h, px)| std::fs::write(path, bmp(*w, *h, px)).is_ok());
    let (w, h) = r.map_or((0, 0), |(w, h, _)| (*w, *h));
    f.event(&json!({"event": "shot", "path": path, "ok": ok, "width": w, "height": h}));
}

/// A 32-bit top-down BMP of BGRA pixels.
fn bmp(w: u32, h: u32, pixels: &[u32]) -> Vec<u8> {
    let data = w * h * 4;
    let mut out = Vec::with_capacity(54 + data as usize);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(54 + data).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(-(h as i32)).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&data.to_le_bytes());
    out.extend_from_slice(&[0; 16]);
    for p in pixels {
        out.extend_from_slice(&p.to_le_bytes());
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Opening a Terminal in a window.

/// Shows a Terminal (or just a window) of `endpoint`: an existing window of this Host is asked
/// first (front to back); without one, a new `subak-gui` is started for it. The foreground is
/// handed on with `AllowSetForegroundWindow`.
fn open_in_window(endpoint: &str, entry: Option<&Entry>, test: bool, secs: u32) -> Value {
    let id = entry.map(|e| e.id.clone());
    let probe = json!({"op": "probe", "endpoint": endpoint});
    for hwnd in windows_of_class(WINDOW_CLASS) {
        if send_copydata(hwnd, COPYDATA_OPEN, &probe) != 1 {
            continue;
        }
        let pid = window_pid(hwnd);
        // SAFETY: lets that window's process take the foreground for the user's choice; the
        // window calls SetForegroundWindow for itself.
        let allowed = unsafe { AllowSetForegroundWindow(pid) } != 0;
        let body = json!({"op": "open", "endpoint": endpoint, "terminal": id});
        if send_copydata(hwnd, COPYDATA_OPEN, &body) == 1 {
            return json!({"event": "opened", "how": "existing", "hwnd": hwnd as usize, "pid": pid,
                "terminal": id, "allow_foreground": allowed});
        }
    }
    let Ok(exe) = std::env::current_exe() else {
        return json!({"event": "open_failed", "error": "cannot find subak-gui"});
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["--endpoint", endpoint]);
    if test {
        // A test window, like this one: off-screen, never activated, its events on our stdout.
        cmd.args(["--test-offscreen", "--test-max-seconds", &secs.to_string()]);
        cmd.stdout(Stdio::inherit());
    } else {
        cmd.stdout(Stdio::null());
    }
    if let Some(t) = &id {
        cmd.arg(t);
    }
    match cmd.stdin(Stdio::null()).stderr(Stdio::null()).spawn() {
        Ok(child) => {
            let pid = child.id();
            // SAFETY: as above, for the window this process just started for the user.
            let allowed = unsafe { AllowSetForegroundWindow(pid) } != 0;
            json!({"event": "opened", "how": "spawned", "pid": pid, "terminal": id,
                "allow_foreground": allowed})
        }
        Err(e) => json!({"event": "open_failed", "error": e.to_string()}),
    }
}

// ---------------------------------------------------------------------------------------------
// The window procedure and the message loop.

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let mut after = Vec::new();
    let handled = FLOAT.with(|cell| {
        let Ok(mut slot) = cell.try_borrow_mut() else {
            return None;
        };
        let f = slot.as_mut()?;
        let r = handle(f, hwnd, msg, wp, lp);
        after = std::mem::take(&mut f.after);
        r
    });
    run_after(after);
    // SAFETY: default handling of everything not handled above.
    handled.unwrap_or_else(|| unsafe { DefWindowProcW(hwnd, msg, wp, lp) })
}

fn flush_after() {
    let after = FLOAT.with_borrow_mut(|slot| {
        slot.as_mut()
            .map(|f| std::mem::take(&mut f.after))
            .unwrap_or_default()
    });
    run_after(after);
}

/// Runs the queued [`After`] actions (the state must not be borrowed).
fn run_after(after: Vec<After>) {
    for a in after {
        // SAFETY: Win32 calls on our own windows (and WM_COPYDATA requests to Subak windows),
        // made with the state free (they send messages back).
        unsafe {
            match a {
                After::Place {
                    hwnd,
                    rect,
                    topmost,
                } => {
                    let (z, flags) = match topmost {
                        Some(true) => (HWND_TOPMOST, SWP_NOACTIVATE),
                        Some(false) => (HWND_NOTOPMOST, SWP_NOACTIVATE),
                        None => (null_mut(), SWP_NOACTIVATE | SWP_NOZORDER),
                    };
                    SetWindowPos(
                        hwnd,
                        z,
                        rect.left,
                        rect.top,
                        rect.width(),
                        rect.height(),
                        flags,
                    );
                }
                After::Present(pic, pos) => {
                    let hwnd = FLOAT.with_borrow(|s| s.as_ref().map(|f| f.icon));
                    if let Some(hwnd) = hwnd {
                        present(hwnd, &pic, pos);
                    }
                }
                After::Show(hwnd, cmd) => {
                    ShowWindow(hwnd, cmd);
                }
                After::Focus(hwnd) => {
                    SetFocus(hwnd);
                }
                After::Foreground(hwnd) => {
                    if IsIconic(hwnd) != 0 {
                        ShowWindow(hwnd, SW_RESTORE);
                    }
                    SetForegroundWindow(hwnd);
                }
                After::CloseUi(ui) => {
                    DestroyWindow(ui.hwnd);
                    drop(ui); // the fonts and brushes go after the controls
                }
                After::Menu { owner, items, at } => {
                    let menu = CreatePopupMenu();
                    if menu.is_null() {
                        continue;
                    }
                    for (id, label, flags) in &items {
                        if *id == 0 {
                            AppendMenuW(menu, MF_SEPARATOR, 0, null());
                        } else {
                            let w = wide(label);
                            AppendMenuW(menu, MF_STRING | flags, *id as usize, w.as_ptr());
                        }
                    }
                    // The documented way for a menu of a window that is not active: make it
                    // the foreground (the user just clicked it) so the menu closes on a click
                    // elsewhere.
                    SetForegroundWindow(owner);
                    let cmd = TrackPopupMenuEx(
                        menu,
                        TPM_RETURNCMD | TPM_RIGHTBUTTON,
                        at.0,
                        at.1,
                        owner,
                        null(),
                    );
                    PostMessageW(owner, WM_NULL, 0, 0);
                    DestroyMenu(menu);
                    if cmd > 0 {
                        PostMessageW(owner, WM_MENU, cmd as usize, 0);
                    }
                }
                After::Open(entry, close) => {
                    let info = FLOAT.with_borrow(|s| {
                        s.as_ref()
                            .map(|f| (f.endpoint.clone(), f.opts.test, f.opts.test_max_seconds))
                    });
                    let Some((endpoint, test, secs)) = info else {
                        continue;
                    };
                    let v = open_in_window(&endpoint, entry.as_ref(), test, secs);
                    let more = FLOAT.with_borrow_mut(|s| {
                        let f = s.as_mut()?;
                        f.event(&v);
                        f.opened.push(v);
                        if close {
                            f.close_manager();
                        }
                        Some(std::mem::take(&mut f.after))
                    });
                    run_after(more.unwrap_or_default());
                }
                After::Shot { hwnd, path } => {
                    let r = (!hwnd.is_null()).then(|| shot_window(hwnd)).flatten();
                    FLOAT.with_borrow(|s| {
                        if let Some(f) = s.as_ref() {
                            shot_event(f, &path, r.as_ref());
                        }
                    });
                }
                After::Quit => {
                    let (more, icon) = FLOAT
                        .with_borrow_mut(|s| {
                            let f = s.as_mut()?;
                            f.close_manager();
                            f.tray.delete();
                            if let Some(l) = f.link.take() {
                                l.close();
                            }
                            f.event(&json!({"event": "closing"}));
                            Some((std::mem::take(&mut f.after), f.icon))
                        })
                        .unwrap_or_default();
                    run_after(more);
                    if !icon.is_null() {
                        DestroyWindow(icon);
                    }
                }
            }
        }
    }
}

/// Hands the icon's picture to `UpdateLayeredWindow` (per-pixel alpha) at `pos`.
fn present(hwnd: HWND, pic: &Picture, pos: (i32, i32)) {
    let size = pic.size as i32;
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size,
            biHeight: -size,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..BITMAPINFOHEADER::default()
        },
        ..BITMAPINFO::default()
    };
    // SAFETY: a DIB section of the picture's size, filled within bounds, selected into a memory
    // DC for the call and freed afterwards; our own layered window.
    unsafe {
        let screen = GetDC(null_mut());
        let mem = CreateCompatibleDC(screen);
        let mut bits: *mut core::ffi::c_void = null_mut();
        let dib = CreateDIBSection(
            mem,
            &raw const bmi,
            DIB_RGB_COLORS,
            &raw mut bits,
            null_mut(),
            0,
        );
        if !dib.is_null() && !bits.is_null() {
            std::slice::from_raw_parts_mut(bits.cast::<u32>(), pic.pixels.len())
                .copy_from_slice(&pic.pixels);
            let old = SelectObject(mem, dib as HGDIOBJ);
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let pt = POINT { x: pos.0, y: pos.1 };
            let sz = SIZE { cx: size, cy: size };
            let src = POINT { x: 0, y: 0 };
            UpdateLayeredWindow(
                hwnd,
                screen,
                &raw const pt,
                &raw const sz,
                mem,
                &raw const src,
                0,
                &raw const blend,
                ULW_ALPHA,
            );
            SelectObject(mem, old);
            DeleteObject(dib as HGDIOBJ);
        }
        DeleteDC(mem);
        ReleaseDC(null_mut(), screen);
    }
}

/// One message with the state borrowed. `None`: default handling.
fn handle(f: &mut Float, hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    let is_icon = hwnd == f.icon;
    let is_manager = f.ui.as_ref().is_some_and(|u| u.hwnd == hwnd);
    if msg == f.taskbar_created && f.taskbar_created != 0 {
        // Explorer restarted: the tray icon is shown again.
        if f.prefs.tray {
            f.tray.delete();
            f.refresh_icon(true);
        }
        return Some(0);
    }
    match msg {
        WM_NET => {
            f.on_net();
            Some(0)
        }
        WM_TRAY if is_icon => {
            f.on_tray(wp, lp);
            Some(0)
        }
        WM_MENU => {
            f.menu_command(wp as u32);
            Some(0)
        }
        WM_CLOSE_MANAGER => {
            f.close_manager();
            Some(0)
        }
        WM_LBUTTONDOWN | WM_LBUTTONUP | WM_MOUSEMOVE | WM_RBUTTONUP if is_icon => {
            let (x, y) = lparam_xy(lp);
            f.on_mouse(msg, wp, x, y);
            Some(0)
        }
        WM_MOUSELEAVE if is_icon => {
            f.hovering = false;
            Some(0)
        }
        0x0215 /* WM_CAPTURECHANGED */ if is_icon => {
            if let Some(d) = f.drag.take()
                && d.moved
            {
                f.prefs.position = Some(Position {
                    x: f.pos.0,
                    y: f.pos.1,
                    dpi: f.dpi,
                });
                f.save_prefs();
            }
            Some(0)
        }
        WM_DPICHANGED if is_icon => {
            if !f.opts.test {
                // SAFETY: `lp` points at the suggested RECT Windows passes with this message.
                let r = unsafe { &*(lp as *const RECT) };
                f.on_dpi((wp & 0xFFFF) as u32, Some((r.left, r.top)));
            }
            Some(0)
        }
        WM_DPICHANGED if is_manager => {
            if !f.opts.test {
                // SAFETY: as above.
                let r = rect_of(unsafe { &*(lp as *const RECT) });
                let dpi = (wp & 0xFFFF) as u32;
                if let Some(ui) = &mut f.ui {
                    ui.dpi = dpi;
                    ui.paint = Paint::new(dpi, f.hc);
                }
                f.after.push(After::Place {
                    hwnd,
                    rect: r,
                    topmost: None,
                });
                f.set_fonts();
                f.layout_manager();
                f.sync_ui();
            }
            Some(0)
        }
        WM_DISPLAYCHANGE => {
            f.on_display_change();
            Some(0)
        }
        WM_SETTINGCHANGE | WM_SYSCOLORCHANGE | WM_THEMECHANGED => {
            if msg == WM_SETTINGCHANGE && wp == SPI_SETWORKAREA {
                f.on_display_change();
            }
            let hc = high_contrast_on();
            if hc != f.hc || msg == WM_SYSCOLORCHANGE {
                f.hc = hc;
                if let Some(ui) = &mut f.ui {
                    ui.paint = Paint::new(ui.dpi, hc);
                }
                f.set_fonts();
                f.sync_ui();
                f.refresh_icon(true);
            }
            None
        }
        WM_TIMER if is_icon => {
            match wp {
                TIMER_RECONNECT => {
                    // SAFETY: our own one-shot timer.
                    unsafe { KillTimer(hwnd, TIMER_RECONNECT) };
                    if !f.connecting {
                        f.connect(false);
                    }
                }
                TIMER_TEST_EXIT => f.after.push(After::Quit),
                _ => {}
            }
            Some(0)
        }
        WM_COPYDATA if is_icon => {
            // SAFETY: `lp` points at the COPYDATASTRUCT of this message.
            let kind = unsafe { (*(lp as *const COPYDATASTRUCT)).dwData };
            if kind == COPYDATA_OPEN {
                // SAFETY: the lParam of this WM_COPYDATA. A body that is not a valid, small
                // request is not answered.
                return Some(unsafe { copydata(lp, COPYDATA_OPEN) }.map_or(0, |r| f.open_request(&r)));
            }
            if f.opts.test && kind == COPYDATA_TEST {
                // SAFETY: as above.
                match unsafe { copydata(lp, COPYDATA_TEST) } {
                    Some(cmd) => f.test_command(&cmd),
                    None => f.event(&json!({"event": "error", "message": "bad test command"})),
                }
                return Some(1);
            }
            Some(0)
        }
        WM_CLOSE if is_icon => {
            f.after.push(After::Quit);
            Some(0)
        }
        WM_ENDSESSION if is_icon && wp != 0 => {
            f.after.push(After::Quit);
            Some(0)
        }
        WM_DESTROY if is_icon => {
            f.event(&json!({"event": "exit"}));
            // SAFETY: ends this thread's message loop.
            unsafe { PostQuitMessage(0) };
            Some(0)
        }
        // The manager.
        WM_PAINT if is_manager => {
            f.paint_manager(hwnd);
            Some(0)
        }
        WM_ERASEBKGND if is_manager => Some(1),
        WM_PRINTCLIENT if is_manager => {
            f.paint_manager_to(hwnd, wp as HDC);
            Some(0)
        }
        WM_DRAWITEM if is_manager => {
            // SAFETY: `lp` points at the DRAWITEMSTRUCT of this message.
            let d = unsafe { &*(lp as *const DRAWITEMSTRUCT) };
            f.draw_item(d);
            Some(1)
        }
        WM_MEASUREITEM if is_manager => {
            // SAFETY: `lp` points at the MEASUREITEMSTRUCT of this message.
            let m = unsafe { &mut *(lp as *mut MEASUREITEMSTRUCT) };
            m.itemHeight = px(ROW, f.ui.as_ref().map_or(96, |u| u.dpi)) as u32;
            Some(1)
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX if is_manager => {
            f.ctl_color(msg, wp as HDC, lp as HWND)
        }
        WM_COMMAND if is_manager => {
            f.on_command(wp, lp as HWND);
            Some(0)
        }
        WM_CONTEXTMENU if is_manager => {
            let at = (lp != -1).then(|| lparam_xy(lp));
            if let (Some((x, y)), Some(ui)) = (at, &f.ui) {
                let list = ui.ctl(ID_LIST);
                let mut pt = POINT { x, y };
                // SAFETY: maps a screen point into our own list box and asks which row it is.
                let row = unsafe {
                    windows_sys::Win32::Graphics::Gdi::ScreenToClient(list, &raw mut pt);
                    SendMessageW(
                        list,
                        LB_ITEMFROMPOINT,
                        0,
                        ((pt.y as u16 as isize) << 16) | (pt.x as u16 as isize),
                    )
                };
                if (row >> 16) & 0xFFFF == 0 {
                    let fx = f.model.select_index((row & 0xFFFF) as usize);
                    f.apply(fx);
                }
            }
            f.row_menu(at);
            Some(0)
        }
        WM_ACTIVATE if is_manager => {
            if (wp & 0xFFFF) as u32 == WA_INACTIVE && !f.prefs.manager_pinned && !f.opts.test {
                // A flyout: it closes when the user goes elsewhere (after this message).
                // SAFETY: posts to our own window.
                unsafe { PostMessageW(f.icon, WM_CLOSE_MANAGER, 0, 0) };
            }
            None
        }
        WM_CLOSE if is_manager => {
            f.close_manager();
            Some(0)
        }
        _ => None,
    }
}

/// Keys for the manager, before `TranslateMessage` (as the window's shortcuts): returns whether
/// the key was handled. Alt+F4 keeps its meaning (closes the manager).
fn pre_translate(msg: &MSG) -> bool {
    if !matches!(msg.message, WM_KEYDOWN | WM_SYSKEYDOWN | WM_CHAR) {
        return false;
    }
    if msg.message == WM_SYSKEYDOWN && msg.wParam as u32 == vk::F4 {
        return false;
    }
    let handled = FLOAT.with(|cell| {
        let Ok(mut slot) = cell.try_borrow_mut() else {
            return false;
        };
        let Some(f) = slot.as_mut() else {
            return false;
        };
        let Some(ui) = &f.ui else {
            return false;
        };
        // SAFETY: a parent/child test between our own manager and the message's window.
        if msg.hwnd != ui.hwnd && unsafe { IsChild(ui.hwnd, msg.hwnd) } == 0 {
            return false;
        }
        let id = ui.id_of(msg.hwnd).unwrap_or(0);
        if msg.message == WM_CHAR {
            return f.manager_char(id, msg.wParam as u32);
        }
        let lp = msg.lParam as usize;
        let m = held();
        let p = Press {
            vk: msg.wParam as u32,
            scan: ((lp >> 16) & 0xFF) as u32,
            extended: lp & (1 << 24) != 0,
            mods: keys::contract_mods(m),
            ch: None,
            repeat: lp & (1 << 30) != 0,
        };
        f.manager_key(id, &p, m)
    });
    flush_after();
    handled
}

fn sanitize(endpoint: &str) -> String {
    endpoint
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(180)
        .collect()
}

/// Another floating UI of this endpoint runs: ask it to show itself (or its manager), then exit.
fn forward(endpoint: &str, fo: &FloatOptions, test: bool) -> ExitCode {
    let op = if fo.manager { "manager" } else { "show" };
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        for hwnd in windows_of_class(ICON_CLASS) {
            if send_copydata(
                hwnd,
                COPYDATA_OPEN,
                &json!({"op": "probe", "endpoint": endpoint}),
            ) != 1
            {
                continue;
            }
            // SAFETY: lets the running floating UI take the foreground for the user's request.
            let allowed = unsafe { AllowSetForegroundWindow(window_pid(hwnd)) } != 0;
            let body = json!({"op": op, "endpoint": endpoint, "icon": fo.icon, "tray": fo.tray});
            if send_copydata(hwnd, COPYDATA_OPEN, &body) == 1 {
                if test {
                    println!(
                        "{}",
                        json!({"event": "forwarded", "op": op, "to": window_pid(hwnd),
                            "allow_foreground": allowed})
                    );
                }
                return ExitCode::SUCCESS;
            }
        }
        if Instant::now() >= until {
            attach_parent_console();
            eprintln!(
                "subak-gui: error: a floating icon for this endpoint runs but does not answer (conflict)"
            );
            return ExitCode::from(7);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Runs the floating UI until it quits.
pub fn run(o: Options) -> ExitCode {
    // SAFETY: per-monitor DPI awareness v2 before any window exists.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let endpoint = match o.endpoint.clone() {
        Some(e) => e,
        None => match crate::win::pipe::default_endpoint() {
            Ok(e) => e,
            Err(e) => {
                attach_parent_console();
                eprintln!("subak-gui: error: cannot determine the current user: {e} (io)");
                return ExitCode::from(1);
            }
        },
    };
    let fo = o.float.clone().unwrap_or_default();
    let test = o.test;
    // The off-screen test UI never has the keyboard: it reads no real key state.
    keystate::ignore_real_keyboard(test);
    let test_monitors: Option<Vec<Monitor>> = test.then(|| {
        fo.test_monitors
            .clone()
            .unwrap_or_else(|| vec![(OFFSCREEN, OFFSCREEN, OFFSCREEN + 1920, OFFSCREEN + 1040, 96)])
            .into_iter()
            .map(|(l, t, r, b, dpi)| Monitor {
                work: Rect {
                    left: l,
                    top: t,
                    right: r,
                    bottom: b,
                },
                dpi,
            })
            .collect()
    });
    if let Some(m) = &test_monitors
        && m.iter()
            .any(|m| m.work.right > -10_000 && m.work.bottom > -10_000)
    {
        attach_parent_console();
        eprintln!("subak-gui: error: --test-monitors must lie off every real monitor (usage)");
        return ExitCode::from(2);
    }
    // One floating UI per endpoint (and user session): a second one forwards to it.
    let mutex_name = wide(&format!("Local\\mouv-subak-float-{}", sanitize(&endpoint)));
    // SAFETY: a named mutex of this session; closed when this process ends.
    let (mutex, existed): (HANDLE, bool) = unsafe {
        let h = CreateMutexW(null(), 0, mutex_name.as_ptr());
        (h, GetLastError() == ERROR_ALREADY_EXISTS)
    };
    if existed || mutex.is_null() {
        if !mutex.is_null() {
            // SAFETY: our handle to the other instance's mutex.
            unsafe { CloseHandle(mutex) };
        }
        return forward(&endpoint, &fo, test);
    }
    let prefs_dir = crate::gui::prefs::config_dir();
    let loaded = prefs_dir.as_deref().map_or(
        super::fprefs::Loaded {
            prefs: FloatPrefs::default(),
            problem: None,
            read_only: true,
        },
        FloatPrefs::load,
    );
    let mut prefs = loaded.prefs.clone();
    let chose = fo.icon || fo.tray;
    if chose {
        prefs.icon = fo.icon;
        prefs.tray = fo.tray;
    }
    let mons: Vec<Monitor> = test_monitors.clone().unwrap_or_else(|| {
        real_monitors()
            .into_iter()
            .map(|(work, dpi)| Monitor { work, dpi })
            .collect()
    });
    let ((x, y), dpi) = prefs
        .position
        .and_then(|p| place::restore(p.x, p.y, p.dpi, &mons))
        .or_else(|| place::default_place(&mons))
        .unwrap_or(((0, 0), 96));
    let icon_class = wide(ICON_CLASS);
    let manager_class = wide(MANAGER_CLASS);
    // SAFETY: registers our two window classes and creates the icon window (a layered tool
    // popup that never activates; in the test mode on the synthetic, off-screen monitors).
    let icon = unsafe {
        let hinst = GetModuleHandleW(null());
        for (class, style) in [(&icon_class, 0u32), (&manager_class, CS_DBLCLKS)] {
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                style,
                lpfnWndProc: Some(wndproc),
                hInstance: hinst,
                hCursor: LoadCursorW(null_mut(), IDC_ARROW),
                lpszClassName: class.as_ptr(),
                ..WNDCLASSEXW::default()
            };
            RegisterClassExW(&raw const wc);
        }
        let size = place::icon_size(dpi);
        let title = wide("Subak");
        CreateWindowExW(
            WS_EX_LAYERED
                | WS_EX_TOOLWINDOW
                | WS_EX_NOACTIVATE
                | if prefs.on_top { WS_EX_TOPMOST } else { 0 },
            icon_class.as_ptr(),
            title.as_ptr(),
            WS_POPUP,
            x,
            y,
            size,
            size,
            null_mut(),
            null_mut(),
            hinst,
            null(),
        )
    };
    if icon.is_null() {
        attach_parent_console();
        eprintln!(
            "subak-gui: error: cannot create the floating icon: {} (io)",
            std::io::Error::last_os_error()
        );
        return ExitCode::from(1);
    }
    let tray: Box<dyn TrayDebug> = if test {
        Box::new(FakeTray::default())
    } else {
        Box::new(ShellTray {
            hwnd: icon,
            shown: false,
            icon: null_mut(),
        })
    };
    let tc = wide("TaskbarCreated");
    let f = Float {
        matcher: Matcher::load_for(
            Surface::Manager,
            prefs_dir.as_deref(),
            crate::hotkeys::off_reason(o.no_hotkeys),
        ),
        opts: o,
        fopts: fo,
        endpoint,
        prefs,
        prefs_dir,
        prefs_read_only: loaded.read_only,
        icon,
        pos: (x, y),
        // The test mode uses the synthetic monitor's DPI.
        dpi: if test {
            dpi
        } else {
            // SAFETY: our own window.
            unsafe { GetDpiForWindow(icon) }.max(48)
        },
        drag: None,
        hovering: false,
        test_monitors,
        tray,
        tray_anchor: None,
        model: Manager::new(),
        link: None,
        generation: 0,
        connecting: false,
        inbox: {
            let target = icon as isize;
            Inbox::new(Box::new(move || {
                // SAFETY: posting to our window is thread-safe; a closed window drops it.
                unsafe { PostMessageW(target as HWND, WM_NET, 0, 0) };
            }))
        },
        ui: None,
        hc: high_contrast_on(),
        shown_state: None,
        picture: None,
        renders: 0,
        // SAFETY: registers (or looks up) the shell's broadcast message.
        taskbar_created: unsafe { RegisterWindowMessageW(tc.as_ptr()) },
        notice: loaded.problem.clone(),
        after: Vec::new(),
        opened: Vec::new(),
    };
    FLOAT.with_borrow_mut(|slot| *slot = Some(f));
    FLOAT.with_borrow_mut(|slot| {
        if let Some(f) = slot.as_mut() {
            if chose {
                f.save_prefs();
            }
            f.apply_presence();
            if f.opts.test {
                let secs = f.opts.test_max_seconds;
                // SAFETY: our own window's timer.
                unsafe { SetTimer(f.icon, TIMER_TEST_EXIT, secs.saturating_mul(1000), None) };
                f.event(
                    &json!({"event": "ready", "hwnd": f.icon as usize, "pid": std::process::id(),
                    "pos": [f.pos.0, f.pos.1], "size": f.size(), "dpi": f.dpi,
                    "endpoint": f.endpoint, "prefs_problem": loaded.problem}),
                );
            }
            f.connect(false);
            if f.fopts.manager {
                f.open_manager(None);
            }
        }
    });
    flush_after();
    let mut msg = MSG::default();
    // SAFETY: this thread's message loop.
    unsafe {
        while GetMessageW(&raw mut msg, null_mut(), 0, 0) > 0 {
            if pre_translate(&msg) {
                continue;
            }
            TranslateMessage(&raw const msg);
            DispatchMessageW(&raw const msg);
        }
    }
    FLOAT.with_borrow_mut(|slot| {
        if let Some(mut f) = slot.take() {
            f.tray.delete();
            if let Some(l) = f.link.take() {
                l.close();
            }
        }
    });
    // SAFETY: our own mutex handle.
    unsafe { CloseHandle(mutex) };
    ExitCode::SUCCESS
}
