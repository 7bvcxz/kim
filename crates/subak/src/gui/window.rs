//! The Win32 window of `subak-gui` (ADR 0039, ADR 0042, docs/SUBAK_GUI.md): one top-level
//! window, one thread, per-monitor DPI awareness v2, presented with GDI from the `subak-term` CPU
//! renderer's frames (ADR 0036): the tab bar strip at the top (Step 17), the active tab's pane
//! below it. Between events nothing runs: no timer (except the reconnect delay while
//! disconnected and a `hold` shortcut's delay while its key is down), no animation, no GPU
//! context.
//!
//! Message flow: the network threads queue frames and post `WM_NET`; the window feeds them to
//! the pane's [`TermView`], renders, and invalidates only the rows that changed. Keys and the
//! mouse go through the dispatch of ADR 0009 (IME guard, the window's modal parts — the busy
//! confirmation and the rename field —, the shortcuts of the hotkey contract, then the program)
//! and leave as input frames. The tab menu is a native popup menu, opened outside the state
//! borrow (it runs its own message loop); its choice comes back as `WM_MENU`.
//!
//! Re-entrancy: Win32 calls such as `SetWindowPos` send messages back to this window while its
//! state is borrowed. Those calls are queued as [`After`] actions and run once the state is free;
//! a message that still arrives while it is borrowed gets the default handling.
//!
//! **Settings** (Step 20, ADR 0051): `config.toml` ([`crate::config`]) decides the theme, the
//! terminal font chain, text size, line height, padding, window opacity, motion, the mouse and
//! keyboard choices and the shell profiles `Ctrl+T` and the menu start. It is read at start and
//! again when the window gets the focus; the settings menu and the zoom keys edit it line by line.
//! Nothing here is sent to the Host except a new Terminal's explicit command, cwd and env.
//!
//! **Test hooks** (debug builds, or feature `test-hooks`; `--test-offscreen`): the window is
//! created far off every monitor (-20000, -20000) as a tool window (no taskbar button, not in
//! Alt+Tab) with `WS_EX_NOACTIVATE`, shown with `SW_SHOWNOACTIVATE`, so it never appears on the
//! desktop and never takes focus; it never minimizes or maximizes itself; it prints its state as
//! JSON lines on stdout, takes test commands as `WM_COPYDATA`, uses a private clipboard instead
//! of the system one, renders its own frames to BMP files on request (never a capture of the
//! screen), and closes itself after `--test-max-seconds`.

use super::Options;
use super::ask::{Answer, Question, Topic};
use super::clip::{self, Clipboard, FakeClipboard, FakeOpener, FakeSavePicker, Opener};
use super::compose;
use super::export::{self, Export};
use super::find::{Edit, FindBar, Toggle};
use super::keys::{self, Action, Matcher, Outcome, Press, Typed, vk};
use super::layout::{self, Cells, Rect};
use super::model::{Confirm, Effect, Frame as Data, Model, Phase, Want};
use super::motion::{self, Motion as Transitions, What};
use super::net::{self, Inbox, Link, Msg};
use super::prefs::{LastTerminal, Placement, Prefs};
use super::screens::{self, Banner, Choice, Chrome, Hints};
use super::tabs::{self, Hit};
use crate::config::{self, MiddleClick, RightClick, Settings, ThemeChoice};
use kim_hotkeys::keys::Side;
use serde_json::{Value, json};
use std::cell::RefCell;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::ptr::{null, null_mut};
use std::sync::Arc;
use std::time::Instant;
use subak_term::links::{self, Kind};
use subak_term::render::{Frame, RenderOptions, Renderer, Rgb, Theme, html};
use subak_term::{
    Key, Mods, Motion, MouseAction, MouseButton, MouseEvent, MouseOutcome, PasteCheck, SelectMode,
    Span, TermView,
};
use windows_sys::Win32::Foundation::{
    GENERIC_READ, GENERIC_WRITE, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BeginPaint, COLOR_HIGHLIGHT, COLOR_WINDOW,
    COLOR_WINDOWTEXT, ClientToScreen, CreateSolidBrush, DIB_RGB_COLORS, DeleteObject, EndPaint,
    EnumDisplayMonitors, FillRect, GetMonitorInfoW, GetSysColor, HBRUSH, HDC, HMONITOR,
    InvalidateRect, MONITORINFO, PAINTSTRUCT, ScreenToClient, SetDIBitsToDevice,
};
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_WRITE, OPEN_EXISTING};
use windows_sys::Win32::System::Console::{
    ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
    SetStdHandle,
};
use windows_sys::Win32::System::DataExchange::{
    COPYDATASTRUCT, CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows_sys::Win32::System::Ole::CF_UNICODETEXT;
use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows_sys::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use windows_sys::Win32::UI::Controls::Dialogs::{
    GetSaveFileNameW, OFN_EXPLORER, OFN_NOCHANGEDIR, OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST,
    OPENFILENAMEW,
};
use windows_sys::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor,
    GetDpiForWindow, MDT_EFFECTIVE_DPI, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::Input::Ime::{
    CANDIDATEFORM, CFS_CANDIDATEPOS, CFS_POINT, COMPOSITIONFORM, GCS_COMPSTR, GCS_RESULTSTR,
    ImmGetCompositionStringW, ImmGetContext, ImmReleaseContext, ImmSetCandidateWindow,
    ImmSetCompositionWindow,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetDoubleClickTime, GetKeyState, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT,
    ToUnicode, TrackMouseEvent,
};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CREATESTRUCTW, CS_DBLCLKS, CW_USEDEFAULT, CreatePopupMenu, CreateWindowExW,
    DefWindowProcW, DestroyMenu, DestroyWindow, DispatchMessageW, GetClientRect, GetMessageW,
    GetWindowRect, HTCLIENT, IDC_ARROW, IDC_IBEAM, IDI_APPLICATION, IsIconic, IsZoomed, KillTimer,
    LoadCursorW, LoadIconW, MF_GRAYED, MF_SEPARATOR, MF_STRING, MINMAXINFO, MSG, PostMessageW,
    PostQuitMessage, RegisterClassExW, SC_KEYMENU, SIZE_MAXIMIZED, SIZE_MINIMIZED,
    SPI_GETHIGHCONTRAST, SW_RESTORE, SW_SHOWMAXIMIZED, SW_SHOWNOACTIVATE, SW_SHOWNORMAL,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SetCursor, SetForegroundWindow, SetTimer,
    SetWindowPos, SetWindowTextW, ShowWindow, SystemParametersInfoW, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, TrackPopupMenuEx, TranslateMessage, WM_APP, WM_CHAR, WM_CLOSE, WM_COPYDATA,
    WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND, WM_EXITSIZEMOVE, WM_GETMINMAXINFO,
    WM_IME_COMPOSITION, WM_IME_ENDCOMPOSITION, WM_IME_SETCONTEXT, WM_IME_STARTCOMPOSITION,
    WM_KEYDOWN, WM_KEYUP, WM_KILLFOCUS, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCCREATE, WM_PAINT,
    WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETCURSOR, WM_SETFOCUS, WM_SETTINGCHANGE, WM_SIZE, WM_SYSCHAR,
    WM_SYSCOLORCHANGE, WM_SYSCOMMAND, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_THEMECHANGED, WM_TIMER,
    WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_OVERLAPPEDWINDOW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetLayeredWindowAttributes, GetWindowLongPtrW, ICON_BIG, ICON_SMALL, LWA_ALPHA,
    MF_CHECKED, MF_POPUP, SPI_GETCLIENTAREAANIMATION, SendMessageW, SetLayeredWindowAttributes,
    SetWindowLongPtrW, WM_GETICON, WM_SETICON, WS_EX_LAYERED,
};

/// The network threads have queued something.
const WM_NET: u32 = WM_APP + 1;
/// The tab menu's choice (`wParam` the command, `lParam` the tab index or -1).
const WM_MENU: u32 = WM_APP + 2;
const TIMER_RECONNECT: usize = 1;
const TIMER_TEST_EXIT: usize = 2;
const TIMER_SYNC: usize = 3;
/// A `hold` shortcut's delay, armed only while its key is down.
const TIMER_HOLD: usize = 4;
/// Step 20: redraws while a transition runs (never while idle), and hides a toast after 4 s.
const TIMER_MOTION: usize = 5;
const TIMER_TOAST: usize = 6;
/// `WM_MOUSELEAVE` (the pointer left the client area after `TrackMouseEvent`).
const WM_MOUSELEAVE: u32 = 0x02A3;
/// The tab bar's text size (DESIGN_SYSTEM §2.3: UI body 12) and height (§2.4: 32), logical px.
const BAR_FONT: f32 = 12.0;
const BAR_HEIGHT: f32 = 32.0;
/// The tab menu's commands.
const CMD_RENAME: u32 = 1;
const CMD_HIDE: u32 = 2;
const CMD_MOVE_LEFT: u32 = 3;
const CMD_MOVE_RIGHT: u32 = 4;
const CMD_CLOSE: u32 = 5;
const CMD_NEW: u32 = 6;
/// Start the floating icon and Terminal manager (`subak-gui --float`, Step 18).
const CMD_FLOAT: u32 = 7;
/// The pane's commands (Step 19): the Menu key's menu and Ctrl+right click in the pane.
const CMD_COPY: u32 = 10;
const CMD_COPY_HTML: u32 = 11;
const CMD_PASTE: u32 = 12;
const CMD_PASTE_LINE: u32 = 13;
const CMD_SELECT_ALL: u32 = 14;
const CMD_MARK: u32 = 15;
const CMD_FIND: u32 = 16;
const CMD_SAVE: u32 = 17;
const CMD_OPEN_LINK: u32 = 18;
const CMD_COPY_LINK: u32 = 19;
/// Step 20: the settings menu.
const CMD_SETTINGS_EDIT: u32 = 20;
const CMD_SETTINGS_RELOAD: u32 = 21;
/// `CMD_SHOW + k`: show the `k`-th hidden Terminal again.
const CMD_SHOW: u32 = 100;
/// `CMD_SET + k`: the `k`-th choice of the settings menu (a key and its new value).
const CMD_SET: u32 = 200;
/// `CMD_PROFILE + k`: a new Terminal from the `k`-th shell profile.
const CMD_PROFILE: u32 = 400;
/// The terminal font families the settings menu offers (the installed ones are shown).
const MENU_FONTS: [&str; 7] = [
    "Cascadia Mono",
    "Cascadia Code",
    "Consolas",
    "Courier New",
    "Lucida Console",
    "D2Coding",
    "JetBrains Mono",
];
/// `dwData` of a test command (`WM_COPYDATA`): "SUBK".
pub const COPYDATA_TEST: usize = 0x5355_424B;
/// Where the off-screen test window is placed: off every monitor of any real desktop.
const OFFSCREEN: i32 = -20000;
/// `DWMWA_USE_IMMERSIVE_DARK_MODE`, `DWMWA_BORDER_COLOR`, `DWMWA_CAPTION_COLOR`,
/// `DWMWA_TEXT_COLOR` (documented values; the colours are Windows 11 only and fail harmlessly on
/// Windows 10).
const DWMWA_DARK: u32 = 20;
const DWMWA_BORDER: u32 = 34;
const DWMWA_CAPTION: u32 = 35;
const DWMWA_TEXT: u32 = 36;
/// `ISC_SHOWUICOMPOSITIONWINDOW`: the IME's own composition window; the window draws it inline.
const ISC_SHOWUICOMPOSITIONWINDOW: isize = 0x8000_0000;

pub(super) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

pub(super) fn colorref(c: Rgb) -> u32 {
    u32::from(c.0) | (u32::from(c.1) << 8) | (u32::from(c.2) << 16)
}

pub(super) fn rgb_of(colorref: u32) -> Rgb {
    Rgb(
        colorref as u8,
        (colorref >> 8) as u8,
        (colorref >> 16) as u8,
    )
}

/// Prints to the console `subak-gui` was started from, if its output is not redirected.
pub fn attach_parent_console() {
    // SAFETY: plain console and handle calls; handles that exist are left alone.
    unsafe {
        if !GetStdHandle(STD_OUTPUT_HANDLE).is_null() && !GetStdHandle(STD_ERROR_HANDLE).is_null() {
            return;
        }
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return;
        }
        let name = wide("CONOUT$");
        for std in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            if GetStdHandle(std).is_null() {
                let h = CreateFileW(
                    name.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    FILE_SHARE_WRITE,
                    null(),
                    OPEN_EXISTING,
                    0,
                    null_mut(),
                );
                SetStdHandle(std, h);
            }
        }
    }
}

/// One item of a native menu: a command (id > 0), a separator (id 0, no items) or a submenu
/// (items).
#[derive(Debug, Clone, PartialEq, Eq)]
struct MenuItem {
    id: u32,
    label: String,
    enabled: bool,
    checked: bool,
    items: Vec<MenuItem>,
}

impl MenuItem {
    fn cmd(id: u32, label: impl Into<String>, enabled: bool) -> Self {
        Self {
            id,
            label: label.into(),
            enabled,
            checked: false,
            items: Vec::new(),
        }
    }

    fn check(id: u32, label: impl Into<String>, checked: bool) -> Self {
        Self {
            checked,
            ..Self::cmd(id, label, true)
        }
    }

    fn separator() -> Self {
        Self::cmd(0, String::new(), true)
    }

    fn sub(label: impl Into<String>, items: Vec<MenuItem>) -> Self {
        Self {
            items,
            ..Self::cmd(0, label, true)
        }
    }

    fn to_json(&self) -> Value {
        let mut v = json!({"id": self.id, "label": self.label, "enabled": self.enabled});
        if self.checked {
            v["checked"] = json!(true);
        }
        if !self.items.is_empty() {
            v["items"] = Value::Array(self.items.iter().map(MenuItem::to_json).collect());
        }
        v
    }
}

impl From<(u32, String, bool)> for MenuItem {
    fn from((id, label, enabled): (u32, String, bool)) -> Self {
        Self::cmd(id, label, enabled)
    }
}

/// A Win32 call to make once the window's state is no longer borrowed (it sends messages back).
#[derive(Debug)]
enum After {
    /// `SetWindowPos` (screen position and outer size; `None` keeps the position).
    Place(Option<(i32, i32)>, (i32, i32)),
    Title(String),
    Destroy,
    /// Come to the front for the user's choice in the Terminal manager (it granted this process
    /// the foreground with `AllowSetForegroundWindow`); a minimized window is restored.
    Foreground,
    /// The tab menu at a client point (its own message loop): the items `(command, label,
    /// enabled)` (command 0: a separator), for tab `tab` (-1: the bar).
    Menu {
        items: Vec<MenuItem>,
        at: (i32, i32),
        tab: isize,
    },
    /// Step 20: the window's opacity (`WS_EX_LAYERED` with a constant alpha; 255 removes it).
    Opacity(u8),
    /// Step 20: the caption in the theme's colours (dark or light mode, caption, text, border).
    Caption {
        dark: bool,
        colors: [Rgb; 3],
    },
    /// The save dialog for an export the user agreed to (its own modal loop); `hide`: with the
    /// secret-looking values hidden.
    SaveDialog {
        suggested: String,
        hide: bool,
    },
}

/// The tab bar: its renderer (the UI size), the laid-out bar and the composed strip.
#[derive(Debug)]
struct TabBar {
    renderer: Option<Renderer>,
    layout: tabs::Bar,
    strip: Frame,
    /// The strip's height and the row's left inset, in device pixels.
    height: u32,
    pad_x: u32,
    /// The first visible tab (scrolling).
    first: usize,
}

/// The rename field in the active tab (keyboard text capture).
#[derive(Debug, Clone)]
struct RenameUi {
    id: String,
    text: String,
    /// The caret, in characters.
    cursor: usize,
}

/// The confirmation before ending a busy Terminal.
#[derive(Debug, Clone)]
struct ConfirmUi {
    info: Confirm,
    focus: Choice,
    /// The buttons' columns on the banner.
    spans: Vec<(u16, u16, Choice)>,
}

/// A small one-row surface drawn over the pane: the banner, or the IME composition. Its own
/// renderer (and glyph cache), created when first needed.
#[derive(Debug)]
struct Overlay {
    renderer: Renderer,
}

/// A mouse selection in progress.
#[derive(Debug, Clone, Copy)]
struct Drag {
    from: (u16, u16),
    moved: bool,
}

#[derive(Debug)]
struct App {
    hwnd: HWND,
    opts: Options,
    endpoint: String,
    model: Model,
    prefs: Prefs,
    prefs_dir: Option<PathBuf>,
    prefs_read_only: bool,
    /// A one-time notice (an invalid gui.json), shown as a banner until a key is pressed.
    notice: Option<String>,
    /// The pane's view (live, or its last screen).
    pane: TermView,
    /// The state screen's view.
    state_view: TermView,
    renderer: Renderer,
    banner: Option<(Banner, Overlay)>,
    ime: Option<Overlay>,
    composition: String,
    dpi: u32,
    client: (u32, u32),
    grid: (u16, u16),
    minimized: bool,
    maximized: bool,
    focused: bool,
    high_contrast: bool,
    bg_brush: HBRUSH,
    link: Option<Link>,
    generation: u64,
    inbox: Arc<Inbox>,
    high_surrogate: Option<u16>,
    drag: Option<Drag>,
    /// Time, cell and count of the last left click (double and triple clicks).
    clicks: (Option<Instant>, (u16, u16), u32),
    wheel: i32,
    matcher: Matcher,
    bar: TabBar,
    rename: Option<RenameUi>,
    confirm: Option<ConfirmUi>,
    /// The tab list last reported to the test hooks.
    reported_tabs: Value,
    /// The window's last restored (not maximized or minimized) outer rectangle.
    normal: Option<Rect>,
    /// The clipboard, the link opener and the export's save dialog: the system's, or recording
    /// fakes in the off-screen test window (Step 19).
    clipboard: Box<dyn Clipboard>,
    opener: Box<dyn Opener>,
    save_picker: FakeSavePicker,
    /// The search bar (Step 19), while open.
    find: Option<FindBar>,
    /// The bar's caret column, for the IME.
    find_caret: u16,
    /// The pane's view was replaced (a snapshot): the next refresh searches it whole.
    find_stale: bool,
    /// A question before a paste, a link or an export (Step 19).
    ask: Option<Question>,
    /// The export the question is about (kept until it is answered).
    export: Option<Export>,
    /// The link under the pointer when the pane menu opened.
    menu_link: Option<links::Found>,
    title: String,
    frames: u64,
    bytes: u64,
    after: Vec<After>,
    // Step 20: settings, motion and the polished chrome.
    /// The effective settings (`config.toml`).
    settings: Settings,
    /// The settings file as last read (edits start from it).
    cfg: config::Loaded,
    /// Settings changed in this window that could not be saved (an invalid or newer file):
    /// they last while it runs.
    cfg_unsaved: bool,
    /// Windows apps use the light mode (`theme = "system"`).
    light_mode: bool,
    /// Windows shows animations ("Show animations in Windows").
    system_animations: bool,
    motion: Transitions,
    /// The active tab's id at the last bar layout (a change starts the tab transition).
    bar_active: Option<String>,
    /// The tab under the pointer, and whether `WM_MOUSELEAVE` is requested.
    hover: Option<usize>,
    leave_tracked: bool,
    /// The toast shown (a notice that hides by itself after 4 s, unless another replaced it).
    toast: Option<String>,
    /// The settings menu's choices by command (`CMD_SET + k`): key and value.
    menu_choices: Vec<(String, String)>,
    /// The terminal families of [`MENU_FONTS`] that are installed (asked once).
    fonts: Option<Vec<&'static str>>,
    /// The family that draws Latin text in the terminal renderer (the user's, or a fallback).
    face: String,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

pub(super) fn system_theme_colors() -> (Rgb, Rgb, Rgb) {
    // SAFETY: plain system colour queries.
    unsafe {
        (
            rgb_of(GetSysColor(COLOR_WINDOW)),
            rgb_of(GetSysColor(COLOR_WINDOWTEXT)),
            rgb_of(GetSysColor(COLOR_HIGHLIGHT)),
        )
    }
}

pub(super) fn high_contrast_on() -> bool {
    let mut hc = HIGHCONTRASTW {
        cbSize: size_of::<HIGHCONTRASTW>() as u32,
        ..HIGHCONTRASTW::default()
    };
    // SAFETY: `hc` is a correctly sized out-structure.
    let ok =
        unsafe { SystemParametersInfoW(SPI_GETHIGHCONTRAST, hc.cbSize, (&raw mut hc).cast(), 0) };
    ok != 0 && hc.dwFlags & HCF_HIGHCONTRASTON != 0
}

fn make_renderer(theme: Theme, font_size: f32, scale: f32, padding: f32) -> Renderer {
    let opts = RenderOptions {
        scale,
        font_size,
        padding,
        ..RenderOptions::default()
    };
    Renderer::with_system_fonts(theme.clone(), opts)
        .unwrap_or_else(|_| Renderer::builtin(theme, opts))
}

/// A renderer with the user's terminal font chain (`appearance.font_family` first, Step 20).
fn make_term_renderer(theme: Theme, opts: RenderOptions, family: &str) -> Renderer {
    let chain = subak_term::render::font_chain(family);
    Renderer::with_font_chain(theme.clone(), opts, &chain)
        .unwrap_or_else(|_| Renderer::builtin(theme, opts))
}

/// Whether Windows apps use the light mode (`AppsUseLightTheme`; dark when unknown).
pub(super) fn apps_use_light_theme() -> bool {
    let key = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
    let name = wide("AppsUseLightTheme");
    let mut v: u32 = 0;
    let mut size = 4u32;
    // SAFETY: reads one DWORD value of the current user's registry into `v` (4 bytes).
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            (&raw mut v).cast(),
            &raw mut size,
        )
    };
    r == 0 && v != 0
}

/// Whether Windows shows animations ("Show animations in Windows", DESIGN_SYSTEM §4.5).
pub(super) fn system_animations() -> bool {
    let mut on: i32 = 1;
    // SAFETY: a BOOL out-parameter of the documented size.
    let ok =
        unsafe { SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&raw mut on).cast(), 0) };
    ok == 0 || on != 0
}

/// The terminal families of `names` that are installed (the GDI rasterizer finds them).
fn installed_fonts(names: &[&'static str]) -> Vec<&'static str> {
    names
        .iter()
        .copied()
        .filter(|f| subak_term::render::gdi::GdiRasterizer::new(&[f]).is_ok())
        .collect()
}

/// The work areas of the monitors and their DPI.
pub(super) fn monitors() -> Vec<(Rect, u32)> {
    unsafe extern "system" fn each(
        m: HMONITOR,
        _dc: HDC,
        _r: *mut RECT,
        data: LPARAM,
    ) -> windows_sys::core::BOOL {
        // SAFETY: `data` is the Vec passed below, alive for the enumeration.
        let out = unsafe { &mut *(data as *mut Vec<(Rect, u32)>) };
        let mut mi = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..MONITORINFO::default()
        };
        let (mut dx, mut dy) = (96u32, 96u32);
        // SAFETY: valid monitor handle from the enumeration; correctly sized out-structures.
        unsafe {
            if GetMonitorInfoW(m, &raw mut mi) != 0 {
                GetDpiForMonitor(m, MDT_EFFECTIVE_DPI, &raw mut dx, &raw mut dy);
                let w = mi.rcWork;
                out.push((
                    Rect {
                        left: w.left,
                        top: w.top,
                        right: w.right,
                        bottom: w.bottom,
                    },
                    dx.max(48),
                ));
            }
        }
        1
    }
    let mut out: Vec<(Rect, u32)> = Vec::new();
    // SAFETY: the callback only writes to `out`, which outlives the call.
    unsafe {
        EnumDisplayMonitors(null_mut(), null(), Some(each), (&raw mut out) as LPARAM);
    }
    out
}

fn style() -> u32 {
    WS_OVERLAPPEDWINDOW
}

fn ex_style(test: bool) -> u32 {
    if test {
        WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE
    } else {
        0
    }
}

/// The outer size of a window whose client area is `w × h` at `dpi`.
fn outer_for(w: u32, h: u32, dpi: u32, test: bool) -> (i32, i32) {
    let mut rc = RECT {
        left: 0,
        top: 0,
        right: w as i32,
        bottom: h as i32,
    };
    // SAFETY: a plain RECT computation.
    unsafe { AdjustWindowRectExForDpi(&raw mut rc, style(), 0, ex_style(test), dpi) };
    (rc.right - rc.left, rc.bottom - rc.top)
}

fn mods() -> Mods {
    // SAFETY: reads this thread's keyboard state.
    let down = |k: u32| unsafe { GetKeyState(k as i32) } < 0;
    Mods {
        shift: down(vk::SHIFT),
        ctrl: down(vk::CONTROL),
        alt: down(vk::MENU),
    }
}

fn lparam_xy(lp: LPARAM) -> (i32, i32) {
    (
        i32::from(lp as u16 as i16),
        i32::from((lp >> 16) as u16 as i16),
    )
}

impl App {
    /// The theme in use: `kim-dark`, `kim-light` or `high-contrast` (Step 20: the setting, Windows
    /// High Contrast and, for `system`, Windows' light or dark app mode).
    fn theme_name(&self) -> &'static str {
        self.settings
            .appearance
            .theme
            .resolve(self.high_contrast, self.light_mode)
    }

    fn theme(&self) -> Theme {
        match self.theme_name() {
            "high-contrast" => {
                let (w, t, h) = system_theme_colors();
                Theme::high_contrast(w, t, h)
            }
            "kim-light" => Theme::kim_light(),
            _ => Theme::kim_dark(),
        }
    }

    fn hc_theme(&self) -> bool {
        self.theme_name() == "high-contrast"
    }

    /// The chrome colours of the theme in use (the tokens of `kim-dark` or `kim-light`).
    fn chrome(&self) -> Chrome {
        Chrome::for_theme(self.theme_name())
    }

    /// VT text of the window's own parts (written in `kim-dark` tokens) in the theme in use:
    /// reverse video in the system colours for High Contrast, the light tokens for `kim-light`.
    fn themed_vt(&self, vt: &[u8]) -> Vec<u8> {
        if self.hc_theme() {
            screens::high_contrast(vt)
        } else {
            self.chrome().retint(vt)
        }
    }

    /// The terminal's render options from the settings (the scale is the renderer's).
    fn term_opts(&self) -> RenderOptions {
        let a = &self.settings.appearance;
        RenderOptions {
            scale: self.scale(),
            font_size: a.font_size,
            line_height: a.line_height,
            padding: a.padding as f32,
            bold_is_bright: a.bold_is_bright,
        }
    }

    fn cells(&self) -> Cells {
        let m = self.renderer.metrics();
        Cells {
            width: m.width,
            height: m.height,
            pad: self.renderer.padding(),
        }
    }

    fn scale(&self) -> f32 {
        layout::scale(self.dpi)
    }

    /// Test reports on stdout (JSON lines).
    fn event(&self, v: &Value) {
        if self.opts.test {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{v}");
            let _ = out.flush();
        }
    }

    fn invalidate_all(&self) {
        // SAFETY: invalidates our own window.
        unsafe { InvalidateRect(self.hwnd, null(), 0) };
    }

    /// The pane's top edge: the tab bar's height.
    fn top(&self) -> u32 {
        self.bar.height
    }

    fn invalidate_rows(&self, from: u16, to: u16) {
        let c = self.cells();
        let top = self.top();
        let rc = RECT {
            left: 0,
            top: (top + c.pad + u32::from(from) * c.height) as i32,
            right: self.client.0 as i32,
            bottom: (top + c.pad + u32::from(to) * c.height) as i32,
        };
        // SAFETY: invalidates part of our own window.
        unsafe { InvalidateRect(self.hwnd, &raw const rc, 0) };
    }

    fn invalidate_bar(&self) {
        let rc = RECT {
            left: 0,
            top: 0,
            right: self.client.0 as i32,
            bottom: self.top() as i32,
        };
        // SAFETY: invalidates part of our own window.
        unsafe { InvalidateRect(self.hwnd, &raw const rc, 0) };
    }

    // -----------------------------------------------------------------------------------------
    // Drawing.

    /// Renders what changed and invalidates those rows.
    fn redraw(&mut self) {
        let damage = if self.model.phase.shows_pane() {
            self.renderer.render(&self.pane)
        } else {
            self.renderer.render(&self.state_view)
        };
        if damage.is_empty() {
            return;
        }
        self.frames += 1;
        if damage.full {
            self.invalidate_all();
        } else {
            for (a, b) in &damage.rows {
                self.invalidate_rows(*a, *b);
            }
        }
        self.place_ime();
    }

    /// The keys the screens name, as bound now.
    fn hints(&self) -> Hints {
        let k = |a| self.matcher.keys_for(a).map(String::from);
        Hints {
            new: k(Action::NewTerminal),
            close: k(Action::CloseTerminal),
            next: k(Action::NextTab),
        }
    }

    /// Rebuilds the state screen for the current phase and grid.
    fn rebuild_state(&mut self) {
        let (cols, rows) = self.grid;
        let mut v = TermView::new(cols, rows, 0);
        if let Some(vt) =
            screens::screen(&self.model.phase, &self.endpoint, cols, rows, &self.hints())
        {
            v.feed(&self.themed_vt(&vt));
        }
        self.state_view = v;
    }

    /// The tab bar's renderer theme: `bg.raised` with secondary text (the system colours in
    /// High Contrast).
    fn bar_theme(&self) -> Theme {
        let mut theme = self.theme();
        if !self.hc_theme() {
            let c = self.chrome();
            theme.background = c.bg_raised;
            theme.foreground = c.text_secondary;
        }
        theme
    }

    /// Lays out and renders the tab bar (the tabs, their states, the rename field) and composes
    /// its strip.
    fn rebuild_bar(&mut self) {
        let scale = self.scale();
        let hc = self.hc_theme();
        let theme = self.bar_theme();
        let mut r = match self.bar.renderer.take() {
            Some(r) if (r.options().scale - scale).abs() < f32::EPSILON => r,
            _ => make_renderer(theme.clone(), BAR_FONT, scale, 0.0),
        };
        r.set_theme(theme.clone());
        let m = r.metrics();
        let height = (layout::px(BAR_HEIGHT, self.dpi).max(1) as u32).max(m.height + 2);
        let pad_x = layout::px(4.0, self.dpi).max(0) as u32;
        let cols = (self.client.0.saturating_sub(2 * pad_x) / m.width.max(1)).max(1);
        let cols = u16::try_from(cols).unwrap_or(u16::MAX);
        let connected = self.model.connected();
        let items: Vec<tabs::Item<'_>> = self
            .model
            .tabs()
            .iter()
            .map(|t| tabs::Item {
                name: &t.name,
                status: if t.gone() {
                    tabs::Status::Gone
                } else if connected {
                    tabs::Status::from_state(&t.state)
                } else {
                    tabs::Status::Unknown
                },
            })
            .collect();
        let active = self.model.active();
        let edit = self.rename.as_ref().map(|e| tabs::Edit {
            text: &e.text,
            cursor: e.cursor,
        });
        let bar = tabs::layout_hover(
            &items,
            active,
            edit.as_ref(),
            cols,
            self.bar.first,
            self.hover,
        );
        drop(items);
        // Step 20: another tab shown starts the tab transition (the marker fades in).
        let active_id = self.model.terminal().map(|t| t.id.clone());
        if active_id != self.bar_active {
            if self.bar_active.is_some() && active_id.is_some() {
                self.animate(What::Tab);
            }
            self.bar_active = active_id;
        }
        let mut view = TermView::new(cols, 1, 0);
        view.feed(&self.themed_vt(&bar.vt));
        r.render(&view);
        let style = if hc {
            compose::BarStyle {
                fill: theme.background,
                active_fill: None,
                marker: None,
                border: Some(theme.foreground),
            }
        } else {
            let c = self.chrome();
            let t = motion::mix_amount(self.motion.progress(What::Tab, Instant::now()));
            compose::BarStyle {
                fill: c.bg_raised,
                active_fill: Some(c.bg_raised.mix(c.bg_base, t)),
                marker: Some((
                    c.bg_raised.mix(c.accent, t),
                    layout::px(2.0, self.dpi).max(1) as u32,
                )),
                border: Some(c.border_subtle),
            }
        };
        let strip = compose::bar_strip(
            self.client.0,
            height,
            r.frame(),
            pad_x,
            m.width,
            bar.active,
            &style,
        );
        self.bar = TabBar {
            renderer: Some(r),
            first: bar.first,
            layout: bar,
            strip,
            height,
            pad_x,
        };
        self.invalidate_bar();
        self.report_tabs();
    }

    /// Reports the tabs to the test hooks when they changed.
    fn report_tabs(&mut self) {
        if !self.opts.test {
            return;
        }
        let v = self.tabs_json();
        if v != self.reported_tabs {
            self.event(
                &json!({"event": "tabs", "tabs": v["tabs"], "active": v["active"],
                "hidden": v["hidden"]}),
            );
            self.reported_tabs = v;
        }
    }

    fn tabs_json(&self) -> Value {
        let active = self.model.active();
        json!({
            "tabs": self.model.tabs().iter().enumerate().map(|(i, t)| json!({
                "id": t.id, "name": t.name, "state": t.state, "active": Some(i) == active,
                "status": tabs::Status::from_state(&t.state).word(),
            })).collect::<Vec<_>>(),
            "active": active,
            "hidden": self.model.hidden().iter().map(|t| json!({"id": t.id, "name": t.name}))
                .collect::<Vec<_>>(),
        })
    }

    /// Rebuilds the banner: the busy confirmation, else a question (paste, link, export), else
    /// the search bar, else a pending shortcut sequence, else a notice, else the phase's banner.
    fn rebuild_banner(&mut self) {
        enum Content {
            Confirm,
            Ask,
            Find,
            Text(Banner),
        }
        let hc = self.hc_theme();
        let content = if self.confirm.is_some() {
            Some((screens::Tone::Danger, Content::Confirm))
        } else if self.ask.is_some() {
            Some((screens::Tone::Warning, Content::Ask))
        } else if self.find.is_some() {
            Some((screens::Tone::Info, Content::Find))
        } else if let Some(keys) = self.matcher.pending_keys() {
            Some((
                screens::Tone::Info,
                Content::Text(Banner {
                    tone: screens::Tone::Info,
                    text: format!("{keys} … waiting for the next key (Esc cancels)"),
                }),
            ))
        } else if let Some(n) = &self.notice {
            Some((
                screens::Tone::Info,
                Content::Text(Banner {
                    tone: screens::Tone::Info,
                    text: format!("{n} (any key hides this)"),
                }),
            ))
        } else {
            screens::banner(&self.model.phase, &self.hints()).map(|b| (b.tone, Content::Text(b)))
        };
        let Some((tone, content)) = content else {
            if self.banner.take().is_some() {
                self.invalidate_all();
            }
            return;
        };
        let mut theme = self.theme();
        if !hc {
            let c = self.chrome();
            theme.background = c.tone_fill(tone);
            theme.foreground = c.text_primary;
        }
        let shown_before = self.banner.is_some();
        let opts = RenderOptions {
            padding: 4.0,
            line_height: config::LINE_HEIGHT.2,
            ..self.term_opts()
        };
        let mut r = match self.banner.take() {
            Some((_, o)) if o.renderer.options() == opts => o.renderer,
            _ => make_term_renderer(theme.clone(), opts, &self.settings.appearance.font_family),
        };
        r.set_theme(theme);
        r.set_focused(self.focused);
        let m = r.metrics();
        let cols = ((self.client.0.saturating_sub(2 * r.padding())) / m.width.max(1)).max(1);
        let cols = u16::try_from(cols).unwrap_or(u16::MAX);
        let mut view = TermView::new(cols, 1, 0);
        let (vt, b) = match content {
            Content::Confirm => {
                let Some(c) = &mut self.confirm else { return };
                let (vt, spans) = screens::confirm_vt(&c.info, c.focus, cols);
                c.spans = spans;
                (
                    vt,
                    Banner {
                        tone,
                        text: format!("confirm: end `{}`", c.info.name),
                    },
                )
            }
            Content::Ask => {
                let Some(q) = &mut self.ask else { return };
                let vt = q.vt(cols);
                (
                    vt,
                    Banner {
                        tone,
                        text: q.words(),
                    },
                )
            }
            Content::Find => {
                let Some(f) = &mut self.find else { return };
                let (vt, caret) = f.vt(cols);
                self.find_caret = caret;
                (
                    vt,
                    Banner {
                        tone,
                        text: format!("find: {}", f.status()),
                    },
                )
            }
            Content::Text(b) => (screens::banner_vt(&b, cols), b),
        };
        view.feed(&self.themed_vt(&vt));
        r.render(&view);
        self.banner = Some((b, Overlay { renderer: r }));
        // Step 20: a banner that appears fades in and rises 4 px.
        if !shown_before {
            self.animate(What::Banner);
        }
        self.invalidate_all();
        self.place_ime();
    }

    /// Draws the IME composition inline at the cursor (DESIGN_SYSTEM §4.4): an underlined run
    /// on `bg.overlay`, in the terminal font (the tab bar's size while renaming a tab).
    fn rebuild_ime(&mut self) {
        if self.composition.is_empty() {
            if self.ime.take().is_some() {
                self.invalidate_all();
            }
            return;
        }
        let mut theme = self.theme();
        if !self.hc_theme() {
            let c = self.chrome();
            theme.background = c.bg_overlay;
            theme.foreground = c.text_primary;
        }
        let cells: usize = self
            .composition
            .chars()
            .map(|c| usize::from(subak_term::screen::char_width(c)))
            .sum();
        let cols = u16::try_from(cells.max(1)).unwrap_or(u16::MAX);
        let size = if self.rename.is_some() {
            BAR_FONT
        } else {
            self.settings.appearance.font_size
        };
        let mut r = match self.ime.take() {
            Some(o)
                if o.renderer.options().font_size == size
                    && o.renderer.options().scale == self.scale() =>
            {
                o.renderer
            }
            _ => make_renderer(theme.clone(), size, self.scale(), 0.0),
        };
        r.set_theme(theme);
        let mut view = TermView::new(cols, 1, 0);
        view.feed(format!("\x1b[?25l\x1b[4m{}", self.composition).as_bytes());
        r.render(&view);
        self.ime = Some(Overlay { renderer: r });
        self.invalidate_all();
        self.place_ime();
    }

    /// The cursor cell's top-left corner in client pixels (the rename caret while renaming),
    /// and the cell height there.
    fn cursor_at(&self) -> (i32, i32, i32) {
        if self.rename.is_some()
            && let (Some(col), Some(r)) = (self.bar.layout.caret, &self.bar.renderer)
        {
            let m = r.metrics();
            let y = self.bar.height.saturating_sub(m.height) / 2;
            return (
                (self.bar.pad_x + u32::from(col) * m.width) as i32,
                y as i32,
                m.height as i32,
            );
        }
        if self.find.is_some()
            && self.ask.is_none()
            && self.confirm.is_none()
            && let Some((_, o)) = &self.banner
        {
            let m = o.renderer.metrics();
            let pad = o.renderer.padding();
            return (
                (pad + u32::from(self.find_caret) * m.width) as i32,
                (self.top() + pad) as i32,
                m.height as i32,
            );
        }
        let c = self.cells();
        let (col, row) = if self.model.phase.shows_pane() {
            self.pane.cursor_in_view().unwrap_or((0, 0))
        } else {
            (0, 0)
        };
        (
            (c.pad + u32::from(col) * c.width) as i32,
            (self.top() + c.pad + u32::from(row) * c.height) as i32,
            c.height as i32,
        )
    }

    fn cursor_px(&self) -> (i32, i32) {
        let (x, y, _) = self.cursor_at();
        (x, y)
    }

    /// Tells the IME where the cursor is (its candidate list opens below it).
    fn place_ime(&self) {
        let (x, y, h) = self.cursor_at();
        // SAFETY: our window's input context, released below; plain structures.
        unsafe {
            let himc = ImmGetContext(self.hwnd);
            if himc.is_null() {
                return;
            }
            let cf = COMPOSITIONFORM {
                dwStyle: CFS_POINT,
                ptCurrentPos: POINT { x, y },
                ..COMPOSITIONFORM::default()
            };
            ImmSetCompositionWindow(himc, &raw const cf);
            let cand = CANDIDATEFORM {
                dwIndex: 0,
                dwStyle: CFS_CANDIDATEPOS,
                ptCurrentPos: POINT { x, y: y + h },
                ..CANDIDATEFORM::default()
            };
            ImmSetCandidateWindow(himc, &raw const cand);
            ImmReleaseContext(self.hwnd, himc);
        }
    }

    /// The client area as the window shows it, composed in memory: the tab bar, the frame, the
    /// background around it, the banner and the IME composition. What `WM_PAINT` presents, for
    /// test shots.
    fn composed(&self) -> Frame {
        let c = self.chrome();
        let p = self.motion.progress(What::Banner, Instant::now());
        let banner = self.banner.as_ref().map(|(b, o)| compose::BannerLayer {
            frame: o.renderer.frame(),
            fill: o.renderer.theme().background,
            marker: (!self.hc_theme()).then(|| {
                (
                    c.tone_color(b.tone),
                    layout::px(3.0, self.dpi).max(1) as u32,
                )
            }),
            border: c.border_subtle,
            fade: motion::mix_amount(p),
            rise: ((1.0 - p) * motion::RISE_PX * self.scale())
                .round()
                .max(0.0) as u32,
        });
        let (x, y) = self.cursor_px();
        compose::compose(
            self.client,
            self.theme().background,
            Some(&self.bar.strip),
            self.renderer.frame(),
            banner.as_ref(),
            self.ime
                .as_ref()
                .map(|o| (o.renderer.frame(), (x.max(0) as u32, y.max(0) as u32))),
        )
    }

    fn paint(&self) {
        let mut ps = PAINTSTRUCT::default();
        let present = |dc: HDC, f: &Frame, x: i32, y: i32| {
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: f.width as i32,
                    biHeight: -(f.height as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    ..BITMAPINFOHEADER::default()
                },
                ..BITMAPINFO::default()
            };
            // SAFETY: `f.pixels` holds width × height 32-bit pixels, top-down, as described.
            unsafe {
                SetDIBitsToDevice(
                    dc,
                    x,
                    y,
                    f.width,
                    f.height,
                    0,
                    0,
                    0,
                    f.height,
                    f.pixels.as_ptr().cast(),
                    &raw const bmi,
                    DIB_RGB_COLORS,
                );
            }
        };
        let fill = |dc: HDC, x: u32, y: u32, w: u32, h: u32, c: Rgb| {
            if w == 0 || h == 0 {
                return;
            }
            let rc = RECT {
                left: x as i32,
                top: y as i32,
                right: (x + w) as i32,
                bottom: (y + h) as i32,
            };
            // SAFETY: a temporary brush for one rectangle, deleted at once.
            unsafe {
                let b = CreateSolidBrush(colorref(c));
                FillRect(dc, &raw const rc, b);
                DeleteObject(b);
            }
        };
        // Step 20: while a transition runs, the composed client area is presented whole (a few
        // frames over at most 160 ms); otherwise each layer is presented as it is.
        if self.motion.is_running() {
            let f = self.composed();
            // SAFETY: standard paint handling of our own window; the frame lives until EndPaint.
            unsafe {
                let dc = BeginPaint(self.hwnd, &raw mut ps);
                present(dc, &f, 0, 0);
                EndPaint(self.hwnd, &raw const ps);
            }
            return;
        }
        // SAFETY: standard paint handling of our own window; every frame lives in `self`.
        unsafe {
            let dc = BeginPaint(self.hwnd, &raw mut ps);
            let top = self.top();
            present(dc, &self.bar.strip, 0, 0);
            let f = self.renderer.frame();
            present(dc, f, 0, top as i32);
            let (cw, ch) = self.client;
            // The background around the grid (the client area is rarely a whole number of cells).
            let rc_right = RECT {
                left: f.width as i32,
                top: top as i32,
                right: cw as i32,
                bottom: ch as i32,
            };
            FillRect(dc, &raw const rc_right, self.bg_brush);
            let rc_bottom = RECT {
                left: 0,
                top: (top + f.height) as i32,
                right: f.width as i32,
                bottom: ch as i32,
            };
            FillRect(dc, &raw const rc_bottom, self.bg_brush);
            if let Some((b, o)) = &self.banner {
                let bf = o.renderer.frame();
                fill(
                    dc,
                    bf.width,
                    top,
                    cw.saturating_sub(bf.width),
                    bf.height,
                    o.renderer.theme().background,
                );
                present(dc, bf, 0, top as i32);
                if !self.hc_theme() {
                    let c = self.chrome();
                    let marker = layout::px(3.0, self.dpi).max(1) as u32;
                    fill(dc, 0, top, marker, bf.height, c.tone_color(b.tone));
                    fill(
                        dc,
                        0,
                        top + bf.height.saturating_sub(1),
                        cw,
                        1,
                        c.border_subtle,
                    );
                }
            }
            if let Some(o) = &self.ime {
                let (x, y) = self.cursor_px();
                present(dc, o.renderer.frame(), x, y);
            }
            EndPaint(self.hwnd, &raw const ps);
        }
    }

    fn set_theme(&mut self) {
        let theme = self.theme();
        // SAFETY: replaces our own background brush.
        unsafe {
            if !self.bg_brush.is_null() {
                DeleteObject(self.bg_brush);
            }
            self.bg_brush = CreateSolidBrush(colorref(theme.background));
        }
        let c = self.chrome();
        self.after.push(After::Caption {
            dark: c.name != "kim-light",
            colors: [c.bg_base, c.text_secondary, c.border_subtle],
        });
        self.apply_opacity();
        self.renderer.set_theme(theme);
        self.rebuild_state();
        self.rebuild_bar();
        self.rebuild_banner();
        self.rebuild_ime();
        self.redraw();
        self.invalidate_all();
    }

    // -----------------------------------------------------------------------------------------
    // Layout.

    /// The client area changed.
    fn on_size(&mut self, kind: u32, w: u32, h: u32) {
        if kind == SIZE_MINIMIZED || !layout::client_is_usable(w, h) {
            // The pane keeps its size while the window is minimized.
            self.minimized = true;
            return;
        }
        self.minimized = false;
        let was_max = self.maximized;
        self.maximized = kind == SIZE_MAXIMIZED;
        self.client = (w, h);
        self.relayout();
        if !self.maximized {
            self.remember_normal();
        }
        if was_max != self.maximized {
            self.save_prefs();
        }
    }

    /// Recomputes the grid for the client area and the current cells.
    fn relayout(&mut self) {
        // The bar first: its height (a DPI matter) is where the pane starts.
        self.rebuild_bar();
        let (w, h) = self.client;
        let grid = self.cells().grid_for(w, h.saturating_sub(self.top()));
        if grid != self.grid {
            self.grid = grid;
            if matches!(self.model.phase, Phase::Attached { .. }) {
                self.pane.resize(grid.0, grid.1);
            }
            let fx = self.model.on_grid(grid.0, grid.1);
            self.apply(fx);
            self.rebuild_state();
        }
        self.rebuild_banner();
        self.rebuild_ime();
        self.redraw();
        self.invalidate_all();
    }

    fn remember_normal(&mut self) {
        let mut rc = RECT::default();
        // SAFETY: our own window; `rc` is writable.
        unsafe {
            if IsZoomed(self.hwnd) != 0 || IsIconic(self.hwnd) != 0 {
                return;
            }
            GetWindowRect(self.hwnd, &raw mut rc);
        }
        self.normal = Some(Rect {
            left: rc.left,
            top: rc.top,
            right: rc.right,
            bottom: rc.bottom,
        });
    }

    /// A DPI change: re-rasterize at the new scale (never scale bitmaps) and take the size
    /// Windows suggests for the new monitor.
    fn on_dpi(&mut self, dpi: u32, suggested: Option<Rect>) {
        let old = self.dpi;
        self.dpi = dpi.max(48);
        self.renderer.set_scale(self.scale());
        let (pos, size) = match suggested {
            Some(r) => (Some((r.left, r.top)), (r.width(), r.height())),
            None => {
                // A synthetic change (test command): keep the logical size, stay where it is.
                let mut rc = RECT::default();
                // SAFETY: our own window.
                unsafe { GetWindowRect(self.hwnd, &raw mut rc) };
                (
                    None,
                    layout::rescale((rc.right - rc.left, rc.bottom - rc.top), old, self.dpi),
                )
            }
        };
        self.banner = None;
        self.ime = None;
        self.after.push(After::Place(pos, size));
        self.relayout();
    }

    /// The zoom keys (and the settings menu): a new text size, saved in `config.toml`.
    fn set_font_size(&mut self, size: f32) {
        if (size - self.settings.appearance.font_size).abs() < f32::EPSILON {
            return;
        }
        self.settings.appearance.font_size = size;
        self.renderer.set_font_size(size);
        self.banner = None;
        self.ime = None;
        self.relayout();
        self.persist("appearance.font_size", &size.to_string());
    }

    // -----------------------------------------------------------------------------------------
    // Settings (Step 20): config.toml.

    /// Saves one setting in `config.toml` (an in-place line edit from the file as it is now);
    /// an invalid or newer file is never rewritten: the change then lasts while this window
    /// runs, and a notice says so.
    fn persist(&mut self, key: &str, value: &str) {
        let Some(dir) = self.prefs_dir.clone() else {
            return;
        };
        let cfg = config::load(&dir);
        let result = config::set(&cfg, &[(key, value)]).and_then(|e| {
            if e.changes.is_empty() {
                return Ok(false);
            }
            config::write_atomic(&cfg.path, &e.text)
                .map(|()| true)
                .map_err(|io| config::EditError::Layout(io.to_string()))
        });
        match result {
            Ok(written) => {
                self.cfg = config::load(&dir);
                self.event(
                    &json!({"event": "settings_saved", "key": key, "value": value,
                    "written": written, "path": self.cfg.path}),
                );
            }
            Err(e) => {
                self.cfg_unsaved = true;
                self.notice = Some(format!(
                    "{}; the change applies to this window until it closes",
                    e.message()
                ));
                self.event(&json!({"event": "settings_not_saved", "key": key,
                    "error": e.message()}));
                self.rebuild_banner();
            }
        }
    }

    /// Reads `config.toml` again (when the window gets the focus, or "Reload settings") and
    /// applies what changed. Running Terminals are never touched: a profile is used only when a
    /// Terminal is created; a new text size or line height only resizes the shown pane's grid.
    fn reload_settings(&mut self, forced: bool) {
        let Some(dir) = self.prefs_dir.clone() else {
            return;
        };
        let cfg = config::load(&dir);
        if !forced && cfg.text == self.cfg.text && cfg.state() == self.cfg.state() {
            return;
        }
        let notice = cfg.notice();
        let new = cfg.settings().clone();
        self.cfg = cfg;
        self.cfg_unsaved = false;
        self.apply_settings(new);
        self.event(
            &json!({"event": "settings_reloaded", "state": self.cfg.state().as_str(),
            "problem": notice}),
        );
        match notice {
            Some(n) => {
                self.notice = Some(n);
                self.toast = None;
            }
            None if forced => self.toast("Settings reloaded from config.toml"),
            None => {}
        }
        self.rebuild_banner();
    }

    /// Applies new settings: only what changed is redone.
    fn apply_settings(&mut self, new: Settings) {
        let old = std::mem::replace(&mut self.settings, new);
        let (a, b) = (&old.appearance, &self.settings.appearance);
        self.motion.enabled = b.motion.animates(self.system_animations);
        if !self.motion.enabled {
            self.motion.stop();
        }
        let look = a.font_family != b.font_family
            || a.font_size != b.font_size
            || a.line_height != b.line_height
            || a.padding != b.padding
            || a.bold_is_bright != b.bold_is_bright;
        if a.font_family != b.font_family {
            let mut r = make_term_renderer(self.theme(), self.term_opts(), &b.font_family);
            r.set_focused(self.focused);
            self.face = r.face_for('A').family;
            self.renderer = r;
            if !b.font_family.is_empty() && !self.face.eq_ignore_ascii_case(&b.font_family) {
                self.notice = Some(format!(
                    "The font `{}` is not installed; {} is used",
                    b.font_family, self.face
                ));
            }
        } else if look {
            self.renderer.set_options(self.term_opts());
        }
        if old.terminal.word_separators != self.settings.terminal.word_separators {
            self.pane
                .set_word_delimiters(&self.settings.terminal.word_separators);
        }
        if a.theme != b.theme || a.opacity != b.opacity {
            self.set_theme();
        }
        if look {
            self.banner = None;
            self.ime = None;
            self.relayout();
        }
    }

    /// The window's opacity from the setting (High Contrast is always opaque).
    fn apply_opacity(&mut self) {
        let pct = if self.hc_theme() {
            100
        } else {
            self.settings.appearance.opacity
        };
        let alpha = u8::try_from(pct * 255 / 100).unwrap_or(255);
        self.after.push(After::Opacity(alpha));
    }

    /// Starts a transition (none with reduced motion) and the timer that draws it.
    fn animate(&mut self, what: What) {
        let ms = self.motion.start(what, Instant::now());
        self.event(&json!({"event": "motion", "what": what.as_str(), "ms": ms}));
        if ms > 0 {
            // SAFETY: our own window's timer, killed when no transition runs.
            unsafe { SetTimer(self.hwnd, TIMER_MOTION, motion::FRAME_MS, None) };
        }
    }

    /// One frame of the running transitions.
    fn motion_tick(&mut self) {
        let running = self.motion.tick(Instant::now());
        if !running {
            // SAFETY: our own timer.
            unsafe { KillTimer(self.hwnd, TIMER_MOTION) };
            self.event(&json!({"event": "motion_done"}));
        }
        self.rebuild_bar();
        self.invalidate_all();
    }

    /// A short, non-critical message that hides by itself after 4 s (DESIGN_SYSTEM §3.8: a
    /// toast; errors stay until dismissed).
    fn toast(&mut self, text: impl Into<String>) {
        let text = text.into();
        self.event(&json!({"event": "toast", "text": text}));
        self.toast = Some(text.clone());
        self.notice = Some(text);
        // SAFETY: a one-shot timer of our own window.
        unsafe { SetTimer(self.hwnd, TIMER_TOAST, 4000, None) };
        self.rebuild_banner();
    }

    /// The settings menu (Step 20): appearance, mouse and clipboard choices, the file.
    fn settings_menu(&mut self) -> MenuItem {
        let a = self.settings.appearance.clone();
        let t = self.settings.terminal.clone();
        let mut choices: Vec<(String, String)> = Vec::new();
        let mut pick = |key: &str, value: String, label: String, on: bool| {
            choices.push((key.to_string(), value));
            MenuItem::check(CMD_SET + choices.len() as u32 - 1, label, on)
        };
        let themes = ThemeChoice::ALL
            .iter()
            .map(|c| {
                pick(
                    "appearance.theme",
                    c.as_str().into(),
                    c.title().into(),
                    a.theme == *c,
                )
            })
            .collect();
        let size = a.font_size;
        let sizes = vec![
            pick(
                "appearance.font_size",
                Prefs::zoomed(size, 1).to_string(),
                format!("Larger ({} px)", Prefs::zoomed(size, 1)),
                false,
            ),
            pick(
                "appearance.font_size",
                Prefs::zoomed(size, -1).to_string(),
                format!("Smaller ({} px)", Prefs::zoomed(size, -1)),
                false,
            ),
            pick(
                "appearance.font_size",
                config::FONT_SIZE.2.to_string(),
                format!("Default ({} px)", config::FONT_SIZE.2),
                (size - config::FONT_SIZE.2).abs() < f32::EPSILON,
            ),
        ];
        let installed = self
            .fonts
            .get_or_insert_with(|| installed_fonts(&MENU_FONTS))
            .clone();
        let mut fonts = vec![pick(
            "appearance.font_family",
            String::new(),
            "Default (Cascadia Mono, Consolas, …)".into(),
            a.font_family.is_empty(),
        )];
        for f in installed {
            fonts.push(pick(
                "appearance.font_family",
                f.to_string(),
                f.to_string(),
                a.font_family.eq_ignore_ascii_case(f),
            ));
        }
        fonts.push(MenuItem::separator());
        fonts.push(MenuItem::cmd(
            0,
            "Ligatures: not drawn (each cell is drawn on its own, ADR 0051)",
            false,
        ));
        let spacing = [1.0f32, 1.2, 1.4, 1.6]
            .iter()
            .map(|h| {
                pick(
                    "appearance.line_height",
                    h.to_string(),
                    format!("{h:.1}"),
                    (a.line_height - h).abs() < 0.01,
                )
            })
            .collect();
        let opacity = [100u32, 95, 90, 80, 70]
            .iter()
            .map(|o| {
                pick(
                    "appearance.opacity",
                    o.to_string(),
                    format!("{o} %"),
                    a.opacity == *o,
                )
            })
            .collect();
        let motions = [
            (config::Motion::System, "Follow Windows (Show animations)"),
            (config::Motion::Reduced, "Reduced (no motion)"),
            (config::Motion::Full, "Full"),
        ]
        .iter()
        .map(|(m, l)| {
            pick(
                "appearance.motion",
                m.as_str().into(),
                (*l).into(),
                a.motion == *m,
            )
        })
        .collect();
        let rights = [
            (RightClick::CopyPaste, "Copy a selection, else paste"),
            (RightClick::Menu, "Open the pane menu"),
            (RightClick::Paste, "Paste"),
            (RightClick::None, "Nothing"),
        ]
        .iter()
        .map(|(r, l)| {
            pick(
                "terminal.right_click",
                r.as_str().into(),
                (*l).into(),
                t.right_click == *r,
            )
        })
        .collect();
        let items = vec![
            MenuItem::sub("Theme", themes),
            MenuItem::sub(format!("Text size ({size} px)"), sizes),
            MenuItem::sub("Terminal font", fonts),
            MenuItem::sub("Line spacing", spacing),
            MenuItem::sub("Window opacity", opacity),
            MenuItem::sub("Motion", motions),
            MenuItem::separator(),
            pick(
                "terminal.copy_on_select",
                (!t.copy_on_select).to_string(),
                "Copy on select".into(),
                t.copy_on_select,
            ),
            pick(
                "terminal.smart_ctrl_c",
                (!t.smart_ctrl_c).to_string(),
                "Ctrl+C copies a selection".into(),
                t.smart_ctrl_c,
            ),
            MenuItem::sub("Right click", rights),
            pick(
                "terminal.middle_click",
                if t.middle_click == MiddleClick::Paste {
                    "none".into()
                } else {
                    "paste".into()
                },
                "Middle click pastes".into(),
                t.middle_click == MiddleClick::Paste,
            ),
            MenuItem::separator(),
            MenuItem::cmd(CMD_SETTINGS_EDIT, "Edit settings file (config.toml)…", true),
            MenuItem::cmd(CMD_SETTINGS_RELOAD, "Reload settings", true),
        ];
        self.menu_choices = choices;
        MenuItem::sub("Settings", items)
    }

    /// A choice of the settings menu: applied at once and saved in `config.toml`.
    fn choose_setting(&mut self, k: usize) {
        let Some((key, value)) = self.menu_choices.get(k).cloned() else {
            return;
        };
        let mut new = self.settings.clone();
        if let Err(e) = config::set_value(&mut new, &key, &value) {
            self.notice = Some(e.message());
            self.rebuild_banner();
            return;
        }
        self.event(&json!({"event": "setting", "key": key, "value": value}));
        if key == "appearance.font_size" {
            if let Ok(f) = value.parse::<f32>() {
                self.set_font_size(f);
            }
            return;
        }
        self.apply_settings(new);
        self.persist(&key, &value);
        if !self.cfg_unsaved {
            self.toast(format!("{key} = {value} (saved in config.toml)"));
        }
    }

    /// "Edit settings file…": opens `config.toml` in Notepad (created with its header first, so
    /// there is something to edit).
    fn edit_settings(&mut self) {
        let Some(dir) = self.prefs_dir.clone() else {
            return;
        };
        let path = dir.join(config::FILE);
        if !path.exists()
            && let Err(e) = config::write_atomic(&path, &config::new_file_text())
        {
            self.notice = Some(format!("{} cannot be created: {e}", path.display()));
            self.rebuild_banner();
            return;
        }
        match self.opener.edit(&path) {
            Ok(()) => self.toast(format!(
                "Opened {}; the window applies your changes when it gets the focus",
                path.display()
            )),
            Err(e) => {
                self.notice = Some(format!("{} could not be opened: {e}", path.display()));
                self.rebuild_banner();
            }
        }
    }

    /// Creates a Terminal: from shell profile `profile` (by index), or the default profile, or
    /// the Host's defaults. The profile's command, cwd and env are sent explicitly.
    fn new_terminal(&mut self, profile: Option<usize>) {
        let p = match profile {
            Some(i) => self.settings.profiles.get(i).cloned(),
            None => self.settings.default_profile().cloned(),
        };
        self.leave_pane();
        let params = p.as_ref().map(config::Profile::create_params);
        self.event(
            &json!({"event": "new_terminal", "profile": p.as_ref().map(|p| p.name.clone())}),
        );
        let fx = self.model.create_with(params.as_ref());
        self.apply(fx);
    }

    // -----------------------------------------------------------------------------------------
    // Preferences.

    fn save_prefs(&mut self) {
        let Some(dir) = self.prefs_dir.clone() else {
            return;
        };
        if self.prefs_read_only {
            return;
        }
        if let Some(n) = self.normal {
            self.prefs.window = Some(Placement {
                x: n.left,
                y: n.top,
                width: n.width(),
                height: n.height(),
                dpi: self.dpi,
                maximized: self.maximized,
            });
        }
        match self.prefs.save(&dir) {
            Ok(()) => self.event(&json!({"event": "saved", "path": dir.join(super::prefs::FILE)})),
            Err(e) => self.event(&json!({"event": "save_failed", "error": e.to_string()})),
        }
    }

    // -----------------------------------------------------------------------------------------
    // The Host.

    fn connect(&mut self, start: bool) {
        self.generation += 1;
        if let Some(l) = self.link.take() {
            l.close();
        }
        net::connect(
            self.endpoint.clone(),
            start,
            self.generation,
            Arc::clone(&self.inbox),
        );
    }

    fn send(&self, msg: &Value) {
        if let Some(l) = &self.link {
            l.send(msg);
        }
    }

    fn input(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if let (Some(stream), Some(l)) = (self.model.input_stream(), &self.link) {
            l.input(stream, bytes);
        }
    }

    fn apply(&mut self, effects: Vec<Effect>) {
        let mut tabs = false;
        for e in effects {
            match e {
                Effect::Send(m) => self.send(&m),
                Effect::Phase => self.on_phase(),
                Effect::Tabs => tabs = true,
                Effect::ResetView {
                    cols,
                    rows,
                    history,
                } => self.reset_pane(cols, rows, history),
                Effect::Remember(t) => {
                    self.prefs.last_terminal = Some(LastTerminal {
                        id: t.id,
                        name: t.name,
                    });
                    self.save_prefs();
                }
                Effect::Reconnect(d) => {
                    if d.is_zero() {
                        // SAFETY: our own window's timer.
                        unsafe { KillTimer(self.hwnd, TIMER_RECONNECT) };
                        self.connect(false);
                    } else {
                        let ms = u32::try_from(d.as_millis()).unwrap_or(u32::MAX).max(1);
                        // SAFETY: a one-shot use of our own window's timer (killed when it fires).
                        unsafe { SetTimer(self.hwnd, TIMER_RECONNECT, ms, None) };
                    }
                }
                Effect::Connect => self.connect(true),
                Effect::Notice(n) => {
                    self.event(&json!({"event": "notice", "text": n}));
                    self.notice = Some(n);
                    self.rebuild_banner();
                }
                Effect::Confirm(c) => {
                    self.event(&json!({"event": "confirm", "id": c.id, "name": c.name,
                        "program": c.program, "processes": c.processes}));
                    self.matcher.cancel();
                    self.rename = None;
                    self.confirm = Some(ConfirmUi {
                        info: c,
                        focus: Choice::Cancel,
                        spans: Vec::new(),
                    });
                    self.rebuild_banner();
                    tabs = true;
                }
                Effect::Renamed(r) => {
                    self.event(&json!({"event": "renamed", "ok": r.is_ok(),
                        "error": r.as_ref().err()}));
                    match r {
                        Ok(()) => self.rename = None,
                        Err(e) => {
                            self.notice = Some(format!("The name was not changed: {e}"));
                            self.rebuild_banner();
                        }
                    }
                    tabs = true;
                }
            }
        }
        if tabs {
            self.on_tabs();
        }
        // Hidden tabs are kept across runs (Step 18), once the Host's list is known.
        if self.model.connected() {
            let hidden = self.model.hidden_ids();
            if hidden != self.prefs.hidden_terminals {
                self.prefs.hidden_terminals = hidden;
                self.save_prefs();
            }
        }
    }

    /// "Floating icon and Terminal manager" in the tab menu: starts `subak-gui --float` for this
    /// endpoint (a second one only shows the running one). The user's explicit choice.
    fn start_float(&mut self) {
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        let mut cmd = std::process::Command::new(exe);
        cmd.args(["--float", "--endpoint", &self.endpoint]);
        if self.opts.test {
            cmd.args([
                "--test-offscreen",
                "--test-max-seconds",
                &self.opts.test_max_seconds.to_string(),
            ]);
            cmd.stdout(std::process::Stdio::inherit());
        } else {
            cmd.stdout(std::process::Stdio::null());
        }
        let r = cmd
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        match r {
            Ok(child) => self.event(&json!({"event": "float_started", "pid": child.id()})),
            Err(e) => {
                self.notice = Some(format!("The floating icon could not start: {e}"));
                self.rebuild_banner();
            }
        }
    }

    /// A request of the Terminal manager (`WM_COPYDATA` `COPYDATA_OPEN`, Step 18): `probe`
    /// answers whether this window serves `endpoint`; `open` shows the Terminal `terminal` (its
    /// tab, a hidden one first shown again) and brings the window to the front. It never
    /// creates, ends or changes a Terminal.
    fn open_request(&mut self, req: &Value) -> LRESULT {
        if req["endpoint"].as_str() != Some(self.endpoint.as_str()) {
            return 0;
        }
        match req["op"].as_str() {
            Some("probe") => 1,
            Some("open") => {
                if let Some(id) = req["terminal"].as_str() {
                    if self.model.terminal().is_none_or(|t| t.id != id) {
                        self.leave_pane();
                    }
                    let fx = self.model.open_id(id);
                    self.apply(fx);
                }
                self.event(
                    &json!({"event": "open_request", "terminal": req["terminal"],
                    "foreground": if self.opts.test { "skipped (test)" } else { "requested" }}),
                );
                if !self.opts.test {
                    self.after.push(After::Foreground);
                }
                1
            }
            _ => 0,
        }
    }

    /// The tabs changed: drop a rename field or a confirmation whose Terminal is no longer the
    /// shown tab, and redraw the bar.
    fn on_tabs(&mut self) {
        let shown = self.model.terminal().map(|t| t.id.clone());
        if self
            .rename
            .as_ref()
            .is_some_and(|r| Some(&r.id) != shown.as_ref())
        {
            self.rename = None;
        }
        if let Some(c) = &self.confirm {
            let still = self
                .model
                .tabs()
                .iter()
                .any(|t| t.id == c.info.id && t.running());
            if !still || Some(&c.info.id) != shown.as_ref() {
                let name = c.info.name.clone();
                self.confirm = None;
                self.notice = Some(format!("`{name}` is no longer shown; nothing was ended"));
                self.rebuild_banner();
            }
        }
        self.rebuild_bar();
    }

    fn reset_pane(&mut self, cols: u16, rows: u16, history: usize) {
        let mut v = TermView::new(cols, rows, history);
        v.set_answer_reports(true);
        v.set_word_delimiters(&self.settings.terminal.word_separators);
        if !self.focused {
            let _ = v.focus(false);
        }
        self.pane = v;
        self.drag = None;
        self.find_stale = true;
    }

    fn on_phase(&mut self) {
        let p = &self.model.phase;
        let term = self
            .model
            .terminal()
            .map(|t| json!({"id": t.id, "name": t.name, "pid": t.pid}));
        let detail = match p {
            Phase::Attached {
                stream, pane_pid, ..
            } => json!({"stream": stream, "pane_pid": pane_pid}),
            Phase::Failed { code, message, .. } => json!({"code": code, "message": message}),
            Phase::Disconnected {
                attempt, reason, ..
            } => json!({"attempt": attempt, "reason": reason}),
            Phase::Empty { notice } => json!({"notice": notice}),
            Phase::Ended { why, .. } => json!({"why": format!("{why:?}")}),
            Phase::Stored(t) => json!({"state": t.state}),
            _ => Value::Null,
        };
        self.event(
            &json!({"event": "state", "phase": p.name(), "terminal": term, "detail": detail}),
        );
        self.rebuild_state();
        self.rebuild_banner();
        self.update_title();
        self.redraw();
        self.invalidate_all();
    }

    fn update_title(&mut self) {
        let title = match self.model.terminal() {
            Some(t) if self.model.phase.shows_pane() => {
                let program = self.pane.screen().title().trim().to_string();
                if program.is_empty() || program == t.name {
                    format!("{} — Subak", t.name)
                } else {
                    format!("{program} — {} — Subak", t.name)
                }
            }
            _ => "Subak".to_string(),
        };
        if title != self.title {
            self.title = title.clone();
            self.after.push(After::Title(title));
        }
    }

    fn on_net(&mut self) {
        let mut fed = false;
        for (generation, m) in self.inbox.drain() {
            if generation != self.generation {
                if let Msg::Connected { link, .. } = m {
                    link.close();
                }
                continue;
            }
            match m {
                Msg::Connected {
                    link,
                    host_pid,
                    started,
                } => {
                    self.link = Some(link);
                    self.event(
                        &json!({"event": "connected", "host_pid": host_pid, "started": started}),
                    );
                    let fx = self.model.on_connected();
                    self.apply(fx);
                }
                Msg::Failed {
                    code,
                    message,
                    hint,
                } => {
                    let fx = self
                        .model
                        .on_connect_failed(code, &message, hint.as_deref());
                    self.apply(fx);
                }
                Msg::Control(v) => {
                    let fx = self.model.on_control(&v);
                    self.apply(fx);
                }
                Msg::Data {
                    stream,
                    snapshot,
                    bytes,
                } => match self.model.on_data(stream, snapshot) {
                    Data::Ignore => {}
                    Data::Feed => {
                        self.pane.feed(&bytes);
                        self.bytes += bytes.len() as u64;
                        fed = true;
                    }
                    Data::Reset {
                        cols,
                        rows,
                        history,
                    } => {
                        self.reset_pane(cols, rows, history);
                        self.pane.feed(&bytes);
                        self.bytes += bytes.len() as u64;
                        fed = true;
                    }
                },
                Msg::Closed => {
                    self.link = None;
                    let fx = self.model.on_disconnected();
                    self.apply(fx);
                }
            }
        }
        // A snapshot is taken at the pane's size; once it is complete, the view follows the
        // window's grid (the model already asked the Host for it if they differ).
        if matches!(self.model.phase, Phase::Attached { .. })
            && !self.model.in_snapshot()
            && self.pane.size() != self.grid
        {
            self.pane.resize(self.grid.0, self.grid.1);
            fed = true;
        }
        if fed {
            // Step 19: an open search follows the output without moving the view; the current
            // match stays selected if it was, and a selection the user made is left alone.
            if let Some(f) = &mut self.find {
                let was = f.current.map(|c| c.1);
                let selected = was.is_some() && self.pane.selection_span() == was;
                f.refresh(&self.pane, std::mem::take(&mut self.find_stale));
                let (spans, index, cur) = (
                    f.matches().to_vec(),
                    f.current.map(|c| c.0),
                    f.current.map(|c| c.1),
                );
                self.pane.set_highlights(spans, index);
                if selected && let Some(span) = cur {
                    self.pane.select_span(span);
                }
                self.rebuild_banner();
            }
            // Device reports the program asked for: this interactive view answers (ADR 0036).
            let replies = self.pane.take_replies();
            self.input(&replies);
            // Synchronized output (mode 2026): the frame waits until the program ends its
            // update, at most 150 ms (a one-shot timer, only while the mode is on).
            if self.pane.screen().modes().synchronized_output {
                // SAFETY: a one-shot timer of our own window.
                unsafe { SetTimer(self.hwnd, TIMER_SYNC, 150, None) };
            } else {
                self.update_title();
                self.redraw();
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // Keyboard.

    /// A key press. Returns whether it was handled (its character, if any, is then dropped).
    fn key_down(&mut self, p: &Press, m: Mods) -> bool {
        let code = p.vk;
        // 1. IME composition guard (ADR 0009): keys belong to the input method.
        if !self.composition.is_empty() || code == vk::PROCESSKEY {
            return false;
        }
        if matches!(
            code,
            vk::SHIFT
                | vk::CONTROL
                | vk::MENU
                | vk::LSHIFT
                | vk::RSHIFT
                | vk::LCONTROL
                | vk::RCONTROL
                | vk::LMENU
                | vk::RMENU
                | vk::LWIN
                | vk::RWIN
        ) {
            return false;
        }
        if self.notice.take().is_some() {
            self.toast = None;
            self.rebuild_banner();
        }
        // The window's modal parts take the keyboard: nothing reaches the program, no shortcut
        // fires (Alt+F4 still closes the window: it never gets here).
        if self.confirm.is_some() {
            self.confirm_key(code, m);
            return true;
        }
        if self.rename.is_some() {
            return self.rename_key(code, m);
        }
        // Step 19: a question owns the keys; the search bar and mark mode take their own keys
        // first and leave shortcut chords to the matcher.
        if self.ask.is_some() {
            self.ask_key(code, m);
            return true;
        }
        if self.find.is_some()
            && let Some(handled) = self.find_key(code, m)
        {
            return handled;
        }
        if self.pane.marking()
            && let Some(handled) = self.mark_key(code, m)
        {
            return handled;
        }
        // 2. (The Ctrl+B prefix table is Step 23's.) 3. The shortcuts (hotkey contract).
        match self.matcher.press(p) {
            Outcome::Fire(a) => {
                self.rebuild_banner();
                self.run(a);
                return true;
            }
            Outcome::Pending => {
                self.rebuild_banner();
                return true;
            }
            Outcome::Cancelled => {
                self.notice = Some("That key sequence is not a shortcut".into());
                self.rebuild_banner();
                return true;
            }
            Outcome::Swallowed => {
                self.rebuild_banner();
                return true;
            }
            Outcome::Hold(ms) => {
                let ms = u32::try_from(ms).unwrap_or(u32::MAX).max(1);
                // SAFETY: a one-shot timer of our own window, only while the key is down.
                unsafe { SetTimer(self.hwnd, TIMER_HOLD, ms, None) };
                return true;
            }
            Outcome::Ignored => {}
        }
        // The context-menu key opens the shown tab's menu (reserved navigation, §3.5).
        if code == vk::APPS && m == Mods::NONE {
            if self.model.phase.shows_pane() {
                self.menu_link = None;
                self.open_pane_menu(None);
            } else if let Some(i) = self.model.active() {
                self.open_menu(Some(i), None);
            } else {
                self.open_menu(None, None);
            }
            return true;
        }
        // Keys that are neither the search field's nor a shortcut reach nothing while it is open
        // (characters still arrive as WM_CHAR, for the field), and neither do keys that mark
        // mode does not use (a character leaves it and types).
        // (Ctrl+Alt is AltGr on many layouts: its characters still arrive for the field.)
        if self.find.is_some() || self.pane.marking() {
            return keys::special_key(code).is_some() || m.ctrl != m.alt;
        }
        // Step 20 (Tabby's smart Ctrl+C, off by default): Ctrl+C copies a selection; without a
        // selection it reaches the program as always.
        if self.settings.terminal.smart_ctrl_c
            && code == u32::from(b'C')
            && m == Mods::CTRL
            && self.model.phase.shows_pane()
            && self.pane.has_selection()
        {
            self.copy(false);
            return true;
        }
        // 4. The program, or the state screen.
        match self.model.phase {
            Phase::Attached { .. } => {
                if code == vk::F4 && m == Mods::ALT {
                    return false; // Alt+F4 closes the window (a detach).
                }
                let bytes = if let Some(c) = keys::ctrl_digit(code, m) {
                    self.pane.key(Key::Char(c), Mods::CTRL)
                } else if let Some(k) = keys::special_key(code) {
                    self.pane.key(k, m)
                } else {
                    return false; // characters arrive as WM_CHAR
                };
                self.input(&bytes);
                self.redraw();
                true
            }
            _ if code == vk::RETURN => {
                let fx = self.model.activate();
                self.apply(fx);
                true
            }
            _ => false,
        }
    }

    /// A key went up: a `release` shortcut of that key fires.
    fn key_up(&mut self, code: u32) {
        // SAFETY: our own timer; a hold that is released early does nothing.
        unsafe { KillTimer(self.hwnd, TIMER_HOLD) };
        if let Some(a) = self.matcher.release(code) {
            self.run(a);
        }
    }

    /// The busy confirmation's keys: Tab and the arrows move between Cancel and End Terminal,
    /// Enter or Space answers with the focused one, Esc cancels. Everything else is swallowed.
    fn confirm_key(&mut self, code: u32, m: Mods) {
        let Some(c) = &mut self.confirm else {
            return;
        };
        match code {
            vk::ESCAPE => self.answer_confirm(Choice::Cancel),
            vk::TAB | vk::LEFT | vk::RIGHT if !m.ctrl && !m.alt => {
                c.focus = match c.focus {
                    Choice::Cancel => Choice::End,
                    Choice::End => Choice::Cancel,
                };
                self.rebuild_banner();
            }
            vk::RETURN | vk::SPACE if m == Mods::NONE => {
                let choice = c.focus;
                self.answer_confirm(choice);
            }
            _ => {}
        }
    }

    fn answer_confirm(&mut self, choice: Choice) {
        let Some(c) = self.confirm.take() else {
            return;
        };
        self.event(&json!({"event": "confirm_answer", "id": c.info.id,
            "choice": if choice == Choice::End { "end" } else { "cancel" }}));
        self.rebuild_banner();
        if choice == Choice::End {
            let fx = self.model.confirm_close(&c.info.id);
            self.apply(fx);
        }
    }

    /// Starts renaming the shown tab's Terminal (`terminal.rename`, Ctrl+Shift+R).
    fn start_rename(&mut self) {
        let Some(t) = self.model.terminal().cloned() else {
            return;
        };
        if !self.model.connected() || t.gone() {
            self.notice = Some(format!("`{}` cannot be renamed now", t.name));
            self.rebuild_banner();
            return;
        }
        self.matcher.cancel();
        self.rename = Some(RenameUi {
            id: t.id,
            cursor: t.name.chars().count(),
            text: t.name,
        });
        self.event(&json!({"event": "rename", "editing": true}));
        self.rebuild_bar();
        self.place_ime();
    }

    /// The rename field's keys: Enter saves (by id), Esc cancels, Backspace/Delete/arrows/Home/
    /// End edit; characters arrive as `WM_CHAR` (or IME results). Shortcut chords are swallowed.
    fn rename_key(&mut self, code: u32, m: Mods) -> bool {
        let Some(r) = &mut self.rename else {
            return false;
        };
        let n = r.text.chars().count();
        let at = |r: &RenameUi, i: usize| {
            r.text
                .char_indices()
                .nth(i)
                .map_or(r.text.len(), |(b, _)| b)
        };
        match code {
            vk::RETURN => {
                let (id, name) = (r.id.clone(), r.text.trim().to_string());
                if name.is_empty() {
                    self.notice = Some("A Terminal name needs at least one character".into());
                    self.rebuild_banner();
                } else {
                    let fx = self.model.rename(&id, &name);
                    self.apply(fx);
                }
            }
            vk::ESCAPE => {
                self.rename = None;
                self.event(&json!({"event": "rename", "editing": false}));
            }
            vk::BACK if r.cursor > 0 => {
                let (a, b) = (at(r, r.cursor - 1), at(r, r.cursor));
                r.text.replace_range(a..b, "");
                r.cursor -= 1;
            }
            vk::DELETE if r.cursor < n => {
                let (a, b) = (at(r, r.cursor), at(r, r.cursor + 1));
                r.text.replace_range(a..b, "");
            }
            vk::LEFT => r.cursor = r.cursor.saturating_sub(1),
            vk::RIGHT => r.cursor = (r.cursor + 1).min(n),
            vk::HOME => r.cursor = 0,
            vk::END => r.cursor = n,
            vk::BACK | vk::DELETE | vk::TAB | vk::UP | vk::DOWN | vk::PRIOR | vk::NEXT => {}
            _ if m.ctrl || m.alt || keys::special_key(code).is_some() => {}
            // A character key: its WM_CHAR goes into the field.
            _ => return false,
        }
        self.rebuild_bar();
        self.place_ime();
        true
    }

    /// A typed character in the rename field.
    fn rename_char(&mut self, c: char) {
        let Some(r) = &mut self.rename else {
            return;
        };
        if c.is_control() || r.text.chars().count() >= 64 {
            return;
        }
        let b = r
            .text
            .char_indices()
            .nth(r.cursor)
            .map_or(r.text.len(), |(b, _)| b);
        r.text.insert(b, c);
        r.cursor += 1;
        self.rebuild_bar();
        self.place_ime();
    }

    /// A character from the keyboard layout (`WM_CHAR`, `WM_SYSCHAR` with `alt`).
    fn on_char(&mut self, unit: u16, alt: bool, held: Mods) {
        let c = if (0xD800..0xDC00).contains(&unit) {
            self.high_surrogate = Some(unit);
            return;
        } else if (0xDC00..0xE000).contains(&unit) {
            let Some(hi) = self.high_surrogate.take() else {
                return;
            };
            match char::decode_utf16([hi, unit]).next() {
                Some(Ok(c)) => c,
                _ => return,
            }
        } else {
            match char::from_u32(u32::from(unit)) {
                Some(c) => c,
                None => return,
            }
        };
        self.type_char(c, alt, held);
    }

    fn type_char(&mut self, c: char, alt: bool, held: Mods) {
        if self.confirm.is_some() || self.ask.is_some() {
            return;
        }
        if let Some(f) = &mut self.find {
            // Plain and Shift characters, and AltGr's (Ctrl+Alt); not Ctrl or Alt chords.
            let altgr = held.ctrl && held.alt;
            if !alt && (!held.ctrl || altgr) && f.insert(c) {
                self.find_changed();
            }
            return;
        }
        if self.rename.is_some() {
            if !alt {
                self.rename_char(c);
            }
            return;
        }
        if !matches!(self.model.phase, Phase::Attached { .. }) {
            return;
        }
        let bytes = match keys::char_key(c, alt, held) {
            Typed::Key(k, m) => self.pane.key(k, m),
            Typed::Raw(b) => {
                self.pane.scroll_to_bottom();
                self.pane.clear_selection();
                b
            }
        };
        self.input(&bytes);
        self.redraw();
    }

    /// Before the shown tab changes: a program that asked for focus reports learns that it
    /// lost the focus (as when the window loses it).
    fn leave_pane(&mut self) {
        self.rename = None;
        if self.confirm.take().is_some() {
            self.rebuild_banner();
        }
        self.end_find(false);
        if self.ask.take().is_some() {
            self.export = None;
            self.rebuild_banner();
        }
        self.pane.mark_end(false);
        if matches!(self.model.phase, Phase::Attached { .. })
            && let Some(report) = self.pane.focus(false)
        {
            self.input(&report);
        }
    }

    /// Runs a shortcut's (or a menu's) action.
    fn run(&mut self, a: Action) {
        let rows = isize::try_from(self.grid.1).unwrap_or(1).max(1);
        self.event(&json!({"event": "action", "action": a.id()}));
        match a {
            Action::NewTerminal => self.new_terminal(None),
            Action::CloseTerminal => {
                let fx = self.model.close_active();
                self.apply(fx);
            }
            Action::NextTab | Action::PreviousTab => {
                if self.model.tabs().len() > 1 {
                    self.leave_pane();
                }
                let fx = self.model.cycle(if a == Action::NextTab { 1 } else { -1 });
                self.apply(fx);
            }
            Action::HideTab => {
                if let Some(i) = self.model.active() {
                    self.leave_pane();
                    let fx = self.model.hide(i);
                    self.apply(fx);
                }
            }
            Action::MoveTabLeft | Action::MoveTabRight => {
                if let Some(i) = self.model.active() {
                    let d = if a == Action::MoveTabLeft { -1 } else { 1 };
                    let fx = self.model.move_tab(i, d);
                    self.apply(fx);
                }
            }
            Action::RenameTerminal => self.start_rename(),
            Action::Copy => self.copy(false),
            Action::Paste => self.paste(false),
            Action::CopyHtml => self.copy(true),
            Action::PasteOneLine => self.paste(true),
            Action::SelectAll => {
                if self.model.phase.shows_pane() {
                    self.end_find(true);
                    self.pane.select_all();
                    self.redraw();
                }
            }
            Action::MarkMode => self.toggle_mark(),
            Action::FindOpen => self.open_find(),
            Action::SaveOutput => self.ask_export(),
            Action::ZoomIn => {
                self.set_font_size(Prefs::zoomed(self.settings.appearance.font_size, 1));
            }
            Action::ZoomOut => {
                self.set_font_size(Prefs::zoomed(self.settings.appearance.font_size, -1));
            }
            Action::ZoomReset => {
                self.set_font_size(Prefs::zoomed(self.settings.appearance.font_size, 0));
            }
            Action::ScrollPageUp => self.scroll(rows),
            Action::ScrollPageDown => self.scroll(-rows),
            Action::ScrollLineUp => self.scroll(1),
            Action::ScrollLineDown => self.scroll(-1),
            Action::ScrollTop => self.scroll(isize::MAX / 2),
            Action::ScrollBottom => {
                self.pane.scroll_to_bottom();
                self.redraw();
            }
            // The Terminal manager's actions (context `manager`) are never active in a window.
            Action::ManagerOpen
            | Action::ManagerNew
            | Action::ManagerRename
            | Action::ManagerDelete
            | Action::ManagerSearch
            | Action::ManagerPin
            | Action::ManagerClose => {}
        }
    }

    /// Shows tab `i` (a click, the menu).
    fn select_tab(&mut self, i: usize) {
        if self.model.active() == Some(i) {
            return;
        }
        self.leave_pane();
        let fx = self.model.select(i);
        self.apply(fx);
    }

    /// The pane's menu (the Menu key, Ctrl+right click; Step 19): copy, paste, select, find,
    /// save, the link under the pointer, then the shown tab's items. At a client point (`None`:
    /// under the tab).
    fn open_pane_menu(&mut self, at: Option<(i32, i32)>) {
        let keys = |a| {
            self.matcher
                .keys_for(a)
                .map_or_else(String::new, |k| format!("\t{k}"))
        };
        let live = matches!(self.model.phase, Phase::Attached { .. });
        let sel = self.pane.has_selection();
        let mut items: Vec<(u32, String, bool)> = Vec::new();
        if let Some(l) = &self.menu_link {
            let ok = links::validate(&l.target).is_ok();
            let shown: String = l.target.chars().take(60).collect();
            items.push((
                CMD_OPEN_LINK,
                if ok {
                    format!("Open link {shown}")
                } else {
                    format!("Link not opened (only http and https): {shown}")
                },
                ok,
            ));
            items.push((CMD_COPY_LINK, "Copy link".into(), true));
            items.push((0, String::new(), true));
        }
        items.push((CMD_COPY, format!("Copy{}", keys(Action::Copy)), sel));
        items.push((
            CMD_COPY_HTML,
            format!("Copy with colours (HTML){}", keys(Action::CopyHtml)),
            sel,
        ));
        items.push((CMD_PASTE, format!("Paste{}", keys(Action::Paste)), live));
        items.push((
            CMD_PASTE_LINE,
            format!("Paste as one line{}", keys(Action::PasteOneLine)),
            live,
        ));
        items.push((0, String::new(), true));
        items.push((
            CMD_SELECT_ALL,
            format!("Select all{}", keys(Action::SelectAll)),
            true,
        ));
        items.push((
            CMD_MARK,
            format!("Select with the keyboard{}", keys(Action::MarkMode)),
            true,
        ));
        items.push((CMD_FIND, format!("Find…{}", keys(Action::FindOpen)), true));
        items.push((
            CMD_SAVE,
            format!("Save output…{}", keys(Action::SaveOutput)),
            true,
        ));
        items.push((0, String::new(), true));
        let tab = self.model.active();
        self.open_menu_with(items, tab, at);
    }

    /// The tab menu of tab `tab` (or of the bar), at a client point (`None`: under the tab).
    fn open_menu(&mut self, tab: Option<usize>, at: Option<(i32, i32)>) {
        self.open_menu_with(Vec::new(), tab, at);
    }

    /// A menu: `lead` items, then the tab menu of `tab` (or of the bar).
    fn open_menu_with(
        &mut self,
        lead: Vec<(u32, String, bool)>,
        tab: Option<usize>,
        at: Option<(i32, i32)>,
    ) {
        let keys = |a| {
            self.matcher
                .keys_for(a)
                .map_or_else(String::new, |k| format!("\t{k}"))
        };
        let mut items: Vec<(u32, String, bool)> = lead;
        let connected = self.model.connected();
        if let Some(t) = tab.and_then(|i| self.model.tabs().get(i)) {
            let n = self.model.tabs().len();
            let i = tab.unwrap_or(0);
            items.push((
                CMD_RENAME,
                format!("Rename…{}", keys(Action::RenameTerminal)),
                connected && !t.gone(),
            ));
            items.push((
                CMD_HIDE,
                format!(
                    "Hide tab (the Terminal keeps running){}",
                    keys(Action::HideTab)
                ),
                true,
            ));
            items.push((CMD_MOVE_LEFT, "Move left".into(), i > 0));
            items.push((CMD_MOVE_RIGHT, "Move right".into(), i + 1 < n));
            items.push((0, String::new(), true));
            let close = if t.gone() {
                "Close tab".to_string()
            } else if t.stored() {
                "Remove Terminal (nothing runs in it)".to_string()
            } else {
                "Close Terminal (ends it; asks when busy)…".to_string()
            };
            items.push((
                CMD_CLOSE,
                format!("{close}{}", keys(Action::CloseTerminal)),
                connected,
            ));
            items.push((0, String::new(), true));
        }
        items.push((
            CMD_NEW,
            format!("New Terminal{}", keys(Action::NewTerminal)),
            connected,
        ));
        items.push((
            CMD_FLOAT,
            "Floating icon and Terminal manager".to_string(),
            true,
        ));
        if !self.model.hidden().is_empty() {
            items.push((0, String::new(), true));
            for (k, h) in self.model.hidden().iter().enumerate().take(50) {
                items.push((
                    CMD_SHOW + k as u32,
                    format!("Show hidden: {}", h.name),
                    connected,
                ));
            }
        }
        // Step 20: a new Terminal from each shell profile, and the settings menu.
        let mut items: Vec<MenuItem> = items.into_iter().map(MenuItem::from).collect();
        let at_new = items
            .iter()
            .position(|i| i.id == CMD_NEW)
            .map_or(items.len(), |i| i + 1);
        let default = self.settings.default_profile().map(|p| p.name.clone());
        let profiles: Vec<MenuItem> = self
            .settings
            .profiles
            .iter()
            .enumerate()
            .take(64)
            .map(|(k, p)| {
                MenuItem::cmd(
                    CMD_PROFILE + k as u32,
                    format!(
                        "New Terminal: {}{}",
                        p.name,
                        if default.as_deref() == Some(&p.name) {
                            " (default)"
                        } else {
                            ""
                        }
                    ),
                    connected,
                )
            })
            .collect();
        items.splice(at_new..at_new, profiles);
        items.push(MenuItem::separator());
        items.push(self.settings_menu());
        let at = at.unwrap_or_else(|| {
            let x = tab
                .and_then(|i| {
                    self.bar
                        .layout
                        .spans
                        .iter()
                        .find(|s| s.2 == Hit::Tab(i))
                        .map(|s| s.0)
                })
                .unwrap_or(0);
            let w = self.bar.renderer.as_ref().map_or(8, |r| r.metrics().width);
            (
                (self.bar.pad_x + u32::from(x) * w) as i32,
                self.bar.height as i32,
            )
        });
        let tab = tab.map_or(-1, |i| i as isize);
        if self.opts.test {
            // The test window never shows a menu (it would appear on the real desktop).
            self.event(&json!({"event": "menu", "tab": tab, "items": items.iter()
                .map(MenuItem::to_json).collect::<Vec<_>>()}));
            return;
        }
        self.after.push(After::Menu { items, at, tab });
    }

    /// A tab menu choice.
    fn menu_command(&mut self, cmd: u32, tab: Option<usize>) {
        let shown = self.model.active();
        let focus_tab = |app: &mut App| {
            if let Some(i) = tab
                && Some(i) != app.model.active()
            {
                app.select_tab(i);
            }
        };
        match cmd {
            CMD_RENAME => {
                focus_tab(self);
                self.start_rename();
            }
            CMD_HIDE => {
                if let Some(i) = tab.or(shown) {
                    if Some(i) == shown {
                        self.leave_pane();
                    }
                    let fx = self.model.hide(i);
                    self.apply(fx);
                }
            }
            CMD_MOVE_LEFT | CMD_MOVE_RIGHT => {
                if let Some(i) = tab.or(shown) {
                    let d = if cmd == CMD_MOVE_LEFT { -1 } else { 1 };
                    let fx = self.model.move_tab(i, d);
                    self.apply(fx);
                }
            }
            CMD_CLOSE => {
                // "Close Terminal" acts on the shown tab, so the user sees what is ended.
                focus_tab(self);
                self.run(Action::CloseTerminal);
            }
            CMD_NEW => self.run(Action::NewTerminal),
            CMD_FLOAT => self.start_float(),
            CMD_SETTINGS_EDIT => self.edit_settings(),
            CMD_SETTINGS_RELOAD => self.reload_settings(true),
            c if (CMD_PROFILE..CMD_PROFILE + 64).contains(&c) => {
                self.new_terminal(Some((c - CMD_PROFILE) as usize));
            }
            c if (CMD_SET..CMD_PROFILE).contains(&c) => self.choose_setting((c - CMD_SET) as usize),
            CMD_COPY => self.run(Action::Copy),
            CMD_COPY_HTML => self.run(Action::CopyHtml),
            CMD_PASTE => self.run(Action::Paste),
            CMD_PASTE_LINE => self.run(Action::PasteOneLine),
            CMD_SELECT_ALL => self.run(Action::SelectAll),
            CMD_MARK => self.run(Action::MarkMode),
            CMD_FIND => self.run(Action::FindOpen),
            CMD_SAVE => self.run(Action::SaveOutput),
            CMD_OPEN_LINK | CMD_COPY_LINK => {
                if let Some(l) = self.menu_link.take() {
                    if cmd == CMD_COPY_LINK {
                        self.copy_text(&l.target);
                    } else if l.kind == Kind::Hyperlink {
                        // A hyperlink's text may not be its target: shown first.
                        if let Ok(t) = links::validate(&l.target) {
                            self.ask = Some(Question::link(t, Kind::Hyperlink, None));
                            self.rebuild_banner();
                        }
                    } else {
                        self.open_link(&l.target);
                    }
                }
            }
            c if c >= CMD_SHOW => {
                if let Some(h) = self.model.hidden().get((c - CMD_SHOW) as usize).cloned() {
                    self.leave_pane();
                    let fx = self.model.unhide(&h.id);
                    self.apply(fx);
                }
            }
            _ => {}
        }
    }

    fn scroll(&mut self, lines: isize) {
        if self.model.phase.shows_pane() {
            self.pane.scroll_lines(lines);
            self.redraw();
        }
    }

    /// Copies the selection: plain text (CR LF line ends), and with `html` also the HTML Format
    /// with the theme's colours (Step 19). Clears the selection, as Windows Terminal does.
    fn copy(&mut self, html: bool) {
        if !self.model.phase.shows_pane() {
            return;
        }
        let Some(text) = self.pane.selected_text() else {
            return;
        };
        let doc = html
            .then(|| self.pane.selected_cells())
            .flatten()
            .map(|cells| {
                html::cf_html(&html::fragment(
                    &cells,
                    &self.theme(),
                    self.settings.appearance.font_size,
                ))
            });
        if let Err(e) = self.clipboard.write(&clip::crlf(&text), doc.as_deref()) {
            self.notice = Some(format!("Nothing was copied: {e}"));
            self.rebuild_banner();
        }
        self.event(
            &json!({"event": "copied", "chars": text.chars().count(), "html": doc.is_some()}),
        );
        self.pane.mark_end(false);
        self.pane.clear_selection();
        self.redraw();
    }

    /// Copy on select (Step 20): the selection as plain text, kept selected.
    fn copy_keep(&mut self) {
        let Some(text) = self.pane.selected_text() else {
            return;
        };
        if let Err(e) = self.clipboard.write(&clip::crlf(&text), None) {
            self.notice = Some(format!("Nothing was copied: {e}"));
            self.rebuild_banner();
        }
        self.event(
            &json!({"event": "copied", "chars": text.chars().count(), "html": false,
            "on_select": true}),
        );
    }

    /// Pastes the clipboard's plain text (Step 19): into the search field while it is open;
    /// otherwise into the program — after a question when it would run lines, carries control
    /// characters or is large (`SUBAK-SEC-014`). `one_line`: "Paste as one line".
    fn paste(&mut self, one_line: bool) {
        let text = match self.clipboard.read_text() {
            Ok(Some(t)) if !t.is_empty() => t,
            Ok(_) => {
                self.notice = Some("The clipboard holds no text to paste".into());
                self.rebuild_banner();
                return;
            }
            Err(e) => {
                self.notice = Some(format!("The clipboard cannot be read: {e}"));
                self.rebuild_banner();
                return;
            }
        };
        if let Some(f) = &mut self.find {
            if f.insert_str(&text) {
                self.find_changed();
            }
            return;
        }
        if !matches!(self.model.phase, Phase::Attached { .. }) {
            return;
        }
        let text = if one_line {
            subak_term::input::one_line(&text)
        } else {
            text
        };
        let check = PasteCheck::of(&text, self.pane.screen().modes().bracketed_paste);
        self.event(&json!({"event": "paste_check", "breaks": check.breaks,
            "controls": check.controls, "escapes": check.escapes, "bytes": check.bytes,
            "bracketed": check.bracketed, "asks": check.needs_confirmation()}));
        if check.needs_confirmation() {
            self.matcher.cancel();
            self.ask = Some(Question::paste(text, check));
            self.event(&json!({"event": "ask", "words": self.ask.as_ref().map(Question::words)}));
            self.rebuild_banner();
        } else {
            self.paste_text(&text);
        }
    }

    fn paste_text(&mut self, text: &str) {
        let bytes = self.pane.paste(text);
        self.input(&bytes);
        self.redraw();
    }

    // -----------------------------------------------------------------------------------------
    // Questions (Step 19): before a paste, a link, an export.

    /// The question's keys: Tab and the arrows move, Enter or Space answers with the focused
    /// button, Esc cancels. Everything else is swallowed.
    fn ask_key(&mut self, code: u32, m: Mods) {
        let Some(q) = &mut self.ask else {
            return;
        };
        match code {
            vk::ESCAPE => self.answer(Answer::Cancel),
            vk::TAB | vk::RIGHT if !m.ctrl && !m.alt => {
                q.move_focus(code == vk::TAB && m.shift);
                self.rebuild_banner();
            }
            vk::LEFT if !m.ctrl && !m.alt => {
                q.move_focus(true);
                self.rebuild_banner();
            }
            vk::RETURN | vk::SPACE if m == Mods::NONE => {
                let a = q.focused();
                self.answer(a);
            }
            _ => {}
        }
    }

    fn answer(&mut self, a: Answer) {
        let Some(q) = self.ask.take() else {
            return;
        };
        self.event(&json!({"event": "answer", "answer": format!("{a:?}")}));
        self.rebuild_banner();
        match (q.topic, a) {
            (_, Answer::Cancel) => self.export = None,
            (Topic::Paste { text, .. }, Answer::Paste) => self.paste_text(&text),
            (Topic::Paste { text, .. }, Answer::PasteOneLine) => {
                self.paste_text(&subak_term::input::one_line(&text));
            }
            (Topic::Link { target, .. }, Answer::Open) => self.open_link(&target),
            (Topic::Link { target, .. }, Answer::CopyLink) => self.copy_text(&target),
            (Topic::Export { name, .. }, Answer::SaveHidden | Answer::SaveExact) => {
                let hide = a == Answer::SaveHidden;
                let unix = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs());
                let suggested = export::suggested_name(&name, unix);
                if self.opts.test {
                    let path = self.save_picker.pick(&suggested);
                    self.save_export(path, hide);
                } else {
                    self.after.push(After::SaveDialog { suggested, hide });
                }
            }
            _ => {}
        }
    }

    /// Copies `text` (a link's target) as plain text.
    fn copy_text(&mut self, text: &str) {
        match self.clipboard.write(&clip::crlf(text), None) {
            Ok(()) => self.notice = Some(format!("Copied {text}")),
            Err(e) => self.notice = Some(format!("Nothing was copied: {e}")),
        }
        self.event(&json!({"event": "copied", "chars": text.chars().count(), "html": false}));
        self.rebuild_banner();
    }

    // -----------------------------------------------------------------------------------------
    // Links (Step 19).

    /// A Ctrl+click on viewport cell (`col`, `row`): a detected address opens at once (its text
    /// is its target), after validation; a hyperlink shows its target first and opens only on
    /// "Open"; a refused target says why and offers to copy it. Returns whether a link was there.
    fn link_click(&mut self, col: u16, row: u16) -> bool {
        let Some(found) = links::link_at(&self.pane, col, row) else {
            return false;
        };
        let checked = links::validate(&found.target);
        self.event(&json!({"event": "link", "target": found.target,
            "kind": if found.kind == Kind::Hyperlink { "hyperlink" } else { "text" },
            "valid": checked.is_ok()}));
        match (checked, found.kind) {
            (Ok(target), Kind::Text) => self.open_link(&target),
            (Ok(target), Kind::Hyperlink) => {
                self.ask = Some(Question::link(target, Kind::Hyperlink, None));
                self.rebuild_banner();
            }
            (Err(r), kind) => {
                self.ask = Some(Question::link(found.target, kind, Some(r)));
                self.rebuild_banner();
            }
        }
        true
    }

    /// Hands a validated target to the OS "open" handler.
    fn open_link(&mut self, target: &str) {
        let Ok(target) = links::validate(target) else {
            return;
        };
        match self.opener.open(&target) {
            Ok(()) => {
                self.event(&json!({"event": "opened", "verb": "open", "target": target}));
                self.notice = Some(format!("Opened {target}"));
            }
            Err(e) => self.notice = Some(format!("{target} could not be opened: {e}")),
        }
        self.rebuild_banner();
    }

    // -----------------------------------------------------------------------------------------
    // Save output (Step 19).

    /// "Save output…": asks first, naming what the secret detector found.
    fn ask_export(&mut self) {
        if !self.model.phase.shows_pane() {
            return;
        }
        let e = Export::of(&self.pane);
        let name = self
            .model
            .terminal()
            .map_or_else(|| "subak".to_string(), |t| t.name.clone());
        self.matcher.cancel();
        self.ask = Some(Question::export(e.lines.len(), e.bytes(), e.secrets, name));
        self.event(&json!({"event": "ask", "words": self.ask.as_ref().map(Question::words)}));
        self.export = Some(e);
        self.rebuild_banner();
    }

    /// Writes the export the user agreed to, to the path chosen in the save dialog.
    fn save_export(&mut self, path: Option<PathBuf>, hide: bool) {
        let Some(e) = self.export.take() else {
            return;
        };
        let Some(path) = path else {
            self.notice = Some("Nothing was saved".into());
            self.rebuild_banner();
            return;
        };
        match export::write(&path, &e.content(hide)) {
            Ok(()) => {
                self.event(
                    &json!({"event": "saved_output", "path": path, "lines": e.lines.len(),
                    "hidden": hide, "secrets": e.secrets}),
                );
                self.notice = Some(format!(
                    "Saved {} lines to {}{}",
                    e.lines.len(),
                    path.display(),
                    if hide && e.secrets > 0 {
                        " (secret-looking values hidden)"
                    } else {
                        ""
                    }
                ));
            }
            Err(err) => self.notice = Some(format!("{} was not saved: {err}", path.display())),
        }
        self.rebuild_banner();
    }

    // -----------------------------------------------------------------------------------------
    // The search bar (Step 19).

    fn open_find(&mut self) {
        if !self.model.phase.shows_pane() {
            return;
        }
        self.pane.mark_end(true);
        self.matcher.cancel();
        if self.find.is_none() {
            let mut f = FindBar::default();
            // A selection on one line becomes the query.
            if let Some(sel) = self.pane.selected_text()
                && !sel.contains('\n')
                && !sel.trim().is_empty()
            {
                f.insert_str(sel.trim());
            }
            self.find = Some(f);
            self.event(&json!({"event": "find", "open": true}));
            self.find_changed();
        } else if let Some(f) = &mut self.find {
            f.edit(Edit::End);
            self.rebuild_banner();
        }
    }

    /// Closes the search bar; the current match stays selected when `keep`.
    fn end_find(&mut self, keep: bool) {
        if self.find.take().is_none() {
            return;
        }
        self.pane.clear_highlights();
        if !keep {
            self.pane.clear_selection();
        }
        self.event(&json!({"event": "find", "open": false}));
        self.rebuild_banner();
        self.redraw();
    }

    /// The query or an option changed: search again and show the match nearest the bottom of
    /// the view.
    fn find_changed(&mut self) {
        let rows = self.pane.size().1;
        let bottom = self.pane.point_at(0, rows - 1).line;
        let cur = match &mut self.find {
            Some(f) => f.run(&self.pane, bottom),
            None => return,
        };
        self.show_match(cur, true);
        self.report_find();
    }

    fn find_step(&mut self, older: bool) {
        let cur = self.find.as_mut().and_then(|f| f.step(older));
        self.show_match(cur, true);
        self.report_find();
    }

    /// Highlights the matches, selects the current one and (with `reveal`) scrolls to it.
    fn show_match(&mut self, cur: Option<Span>, reveal: bool) {
        let Some(f) = &self.find else {
            return;
        };
        let index = f.current.map(|c| c.0);
        self.pane.set_highlights(f.matches().to_vec(), index);
        match cur {
            Some(span) => {
                self.pane.select_span(span);
                if reveal {
                    self.pane.reveal(span.start.line);
                }
            }
            None => self.pane.clear_selection(),
        }
        self.rebuild_banner();
        self.redraw();
    }

    fn report_find(&self) {
        if let Some(f) = &self.find {
            self.event(&json!({"event": "find_result", "query": f.text,
                "matches": f.matches().len(), "current": f.current.map(|c| c.0),
                "status": f.status()}));
        }
    }

    /// The search field's keys. `None`: not the field's (a shortcut chord may fire).
    fn find_key(&mut self, code: u32, m: Mods) -> Option<bool> {
        let plain = !m.ctrl && !m.alt;
        let edit = |app: &mut App, e: Edit| {
            if let Some(f) = &mut app.find {
                if f.edit(e) {
                    app.find_changed();
                } else {
                    app.rebuild_banner();
                }
            }
            Some(true)
        };
        match code {
            vk::ESCAPE if plain && !m.shift => {
                self.end_find(true);
                Some(true)
            }
            vk::RETURN | vk::F3 if plain => {
                self.find_step(!m.shift);
                Some(true)
            }
            vk::UP | vk::DOWN if plain && !m.shift => {
                self.find_step(code == vk::UP);
                Some(true)
            }
            vk::BACK if m.ctrl && !m.alt && !m.shift => edit(self, Edit::BackWord),
            vk::BACK if plain => edit(self, Edit::Backspace),
            vk::DELETE if plain && !m.shift => edit(self, Edit::Delete),
            vk::LEFT if plain && !m.shift => edit(self, Edit::Left),
            vk::RIGHT if plain && !m.shift => edit(self, Edit::Right),
            vk::HOME if plain && !m.shift => edit(self, Edit::Home),
            vk::END if plain && !m.shift => edit(self, Edit::End),
            0x43 | 0x52 | 0x57 if m.alt && !m.ctrl && !m.shift => {
                let t = match code {
                    0x43 => Toggle::Case,
                    0x52 => Toggle::Regex,
                    _ => Toggle::Word,
                };
                if let Some(f) = &mut self.find {
                    f.toggle(t);
                }
                self.find_changed();
                Some(true)
            }
            // Ctrl+V pastes into the field (the window's paste keys do too, through `run`).
            0x56 if m.ctrl && !m.alt && !m.shift => {
                self.paste(false);
                Some(true)
            }
            _ => None,
        }
    }

    // -----------------------------------------------------------------------------------------
    // Keyboard selection (Step 19, mark mode).

    fn toggle_mark(&mut self) {
        if !self.model.phase.shows_pane() {
            return;
        }
        if self.pane.marking() {
            self.pane.mark_end(false);
        } else {
            self.end_find(true);
            self.matcher.cancel();
            self.pane.mark_begin();
        }
        self.event(&json!({"event": "mark", "on": self.pane.marking()}));
        self.redraw();
    }

    /// Mark mode's keys: the arrows move the caret (Shift extends, Ctrl moves by words),
    /// Home/End, PageUp/PageDown, Ctrl+Home/End; Enter copies and leaves, Esc leaves. `None`:
    /// not one of them (a shortcut chord may fire).
    fn mark_key(&mut self, code: u32, m: Mods) -> Option<bool> {
        if m.alt {
            return None;
        }
        let motion = match (code, m.ctrl) {
            (vk::LEFT, false) => Motion::Left,
            (vk::RIGHT, false) => Motion::Right,
            (vk::LEFT, true) => Motion::WordLeft,
            (vk::RIGHT, true) => Motion::WordRight,
            (vk::UP, false) => Motion::Up,
            (vk::DOWN, false) => Motion::Down,
            (vk::HOME, false) => Motion::LineStart,
            (vk::END, false) => Motion::LineEnd,
            (vk::HOME, true) => Motion::Top,
            (vk::END, true) => Motion::Bottom,
            (vk::PRIOR, false) => Motion::PageUp,
            (vk::NEXT, false) => Motion::PageDown,
            (vk::RETURN, false) if !m.shift => {
                self.copy(false);
                return Some(true);
            }
            (vk::ESCAPE, false) if !m.shift => {
                self.pane.mark_end(false);
                self.event(&json!({"event": "mark", "on": false}));
                self.redraw();
                return Some(true);
            }
            _ => return None,
        };
        self.pane.mark_move(motion, m.shift);
        self.redraw();
        Some(true)
    }

    fn on_focus(&mut self, focused: bool) {
        self.focused = focused;
        self.renderer.set_focused(focused);
        if matches!(self.model.phase, Phase::Attached { .. })
            && let Some(report) = self.pane.focus(focused)
        {
            self.input(&report);
        }
        if focused {
            // Step 20: `subak config …` or an editor may have changed config.toml meanwhile.
            self.reload_settings(false);
            // `subak hotkeys …` may have changed hotkeys.json meanwhile (§8.3 5).
            if self.matcher.reload() {
                self.event(&json!({"event": "hotkeys_reloaded",
                    "state": self.matcher.config_state().as_str(), "problem": self.matcher.problem}));
                if let Some(p) = self.matcher.problem.clone() {
                    self.notice = Some(p);
                }
                self.rebuild_state();
                self.rebuild_banner();
            }
        } else {
            // Focus loss cancels a pending sequence (§5.3).
            self.matcher.cancel();
            // SAFETY: our own timer.
            unsafe { KillTimer(self.hwnd, TIMER_HOLD) };
            self.rebuild_banner();
        }
        self.redraw();
    }

    // -----------------------------------------------------------------------------------------
    // IME.

    fn ime_string(&self, flag: u32) -> Option<String> {
        // SAFETY: our window's input context, released below; the buffer is sized from the
        // byte count the first call reports.
        unsafe {
            let himc = ImmGetContext(self.hwnd);
            if himc.is_null() {
                return None;
            }
            let n = ImmGetCompositionStringW(himc, flag, null_mut(), 0);
            let s = if n > 0 {
                let mut buf = vec![0u16; (n as usize).div_ceil(2)];
                let got = ImmGetCompositionStringW(himc, flag, buf.as_mut_ptr().cast(), n as u32);
                (got > 0).then(|| String::from_utf16_lossy(&buf[..(got as usize) / 2]))
            } else {
                Some(String::new())
            };
            ImmReleaseContext(self.hwnd, himc);
            s
        }
    }

    /// `WM_IME_COMPOSITION`: committed text goes to the program; the text being composed is
    /// drawn inline at the cursor.
    fn on_composition(&mut self, result: Option<String>, composing: Option<String>) {
        if let Some(r) = result {
            for c in r.chars() {
                self.type_char(c, false, Mods::NONE);
            }
        }
        self.composition = composing.unwrap_or_default();
        self.rebuild_ime();
    }

    // -----------------------------------------------------------------------------------------
    // Mouse.

    fn cell_at(&self, x: i32, y: i32, clamp: bool) -> Option<(u16, u16)> {
        let c = self.cells();
        let (cols, rows) = if self.model.phase.shows_pane() {
            self.pane.size()
        } else {
            self.grid
        };
        let gx = x - c.pad as i32;
        let gy = y - self.top() as i32 - c.pad as i32;
        let (col, row) = (
            gx.div_euclid(c.width.max(1) as i32),
            gy.div_euclid(c.height.max(1) as i32),
        );
        let inside = gx >= 0 && gy >= 0 && col < i32::from(cols) && row < i32::from(rows);
        if !inside && !clamp {
            return None;
        }
        Some((
            col.clamp(0, i32::from(cols) - 1) as u16,
            row.clamp(0, i32::from(rows) - 1) as u16,
        ))
    }

    fn mouse_mods(wp: WPARAM) -> Mods {
        Mods {
            shift: wp & 0x0004 != 0,
            ctrl: wp & 0x0008 != 0,
            // SAFETY: reads this thread's keyboard state.
            alt: unsafe { GetKeyState(vk::MENU as i32) } < 0,
        }
    }

    /// A click on the tab bar (`x`, `y` in client pixels inside the strip).
    fn on_bar_mouse(&mut self, msg: u32, x: i32) {
        let w = self.bar.renderer.as_ref().map_or(8, |r| r.metrics().width) as i32;
        let col = (x - self.bar.pad_x as i32).div_euclid(w.max(1));
        let hit = u16::try_from(col).ok().and_then(|c| self.bar.layout.hit(c));
        if let Some(r) = &self.rename
            && !matches!(hit, Some(Hit::Tab(i)) if self.model.tabs().get(i).is_some_and(|t| t.id == r.id))
            && matches!(msg, WM_LBUTTONDOWN | WM_MBUTTONDOWN | WM_RBUTTONDOWN)
        {
            // A click elsewhere ends the rename field without saving.
            self.rename = None;
            self.rebuild_bar();
        }
        self.matcher.cancel();
        match (msg, hit) {
            (WM_LBUTTONDOWN, Some(Hit::Tab(i))) => self.select_tab(i),
            (WM_LBUTTONDBLCLK, Some(Hit::Tab(i))) => {
                self.select_tab(i);
                self.start_rename();
            }
            (WM_LBUTTONUP, Some(Hit::Close(i))) | (WM_MBUTTONUP, Some(Hit::Tab(i))) => {
                if Some(i) == self.model.active() {
                    self.leave_pane();
                }
                let fx = self.model.hide(i);
                self.apply(fx);
            }
            (WM_LBUTTONUP, Some(Hit::New)) => self.run(Action::NewTerminal),
            (WM_LBUTTONDOWN, Some(Hit::Left)) => {
                self.bar.first = self.bar.first.saturating_sub(1);
                self.rebuild_bar();
            }
            (WM_LBUTTONDOWN, Some(Hit::Right)) => {
                self.bar.first += 1;
                self.rebuild_bar();
            }
            (WM_RBUTTONUP, Some(Hit::Tab(i) | Hit::Close(i))) => {
                self.open_menu(Some(i), Some((x, self.bar.height as i32)));
            }
            (WM_RBUTTONUP, _) => self.open_menu(None, Some((x, self.bar.height as i32))),
            _ => {}
        }
    }

    /// A click on the busy confirmation's buttons.
    fn on_confirm_mouse(&mut self, msg: u32, x: i32) -> bool {
        let (Some(c), Some((_, o))) = (&self.confirm, &self.banner) else {
            return false;
        };
        if msg != WM_LBUTTONUP {
            return true;
        }
        let w = o.renderer.metrics().width.max(1) as i32;
        let col = (x - o.renderer.padding() as i32).div_euclid(w);
        let hit = c
            .spans
            .iter()
            .find(|(a, b, _)| i32::from(*a) <= col && col < i32::from(*b))
            .map(|s| s.2);
        if let Some(choice) = hit {
            self.answer_confirm(choice);
        }
        true
    }

    /// The tab under the pointer (Step 20: hover), and `WM_MOUSELEAVE` requested once.
    fn track_hover(&mut self, x: i32, y: i32) {
        let hover = if y >= 0 && y < self.top() as i32 {
            let w = self.bar.renderer.as_ref().map_or(8, |r| r.metrics().width) as i32;
            let col = (x - self.bar.pad_x as i32).div_euclid(w.max(1));
            match u16::try_from(col).ok().and_then(|c| self.bar.layout.hit(c)) {
                Some(Hit::Tab(i) | Hit::Close(i)) => Some(i),
                _ => None,
            }
        } else {
            None
        };
        // The off-screen test window is never under the real pointer, so Windows would report a
        // leave at once; its tests post WM_MOUSELEAVE themselves.
        if !self.leave_tracked && !self.opts.test {
            let mut t = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            // SAFETY: a correctly sized structure for our own window.
            self.leave_tracked = unsafe { TrackMouseEvent(&raw mut t) } != 0;
        }
        if hover != self.hover {
            self.hover = hover;
            self.event(&json!({"event": "hover", "tab": hover}));
            self.rebuild_bar();
        }
    }

    fn on_mouse(&mut self, msg: u32, wp: WPARAM, x: i32, y: i32) {
        let m = Self::mouse_mods(wp);
        let top = self.top() as i32;
        if msg == WM_MOUSEMOVE && self.drag.is_none() {
            self.track_hover(x, y);
        }
        if self.drag.is_none() && y < top {
            self.on_bar_mouse(msg, x);
            return;
        }
        if self.drag.is_none()
            && self.confirm.is_some()
            && let Some((_, o)) = &self.banner
            && y < top + o.renderer.frame().height as i32
            && self.on_confirm_mouse(msg, x)
        {
            return;
        }
        if self.drag.is_none()
            && (self.ask.is_some() || self.find.is_some())
            && let Some((_, o)) = &self.banner
            && y < top + o.renderer.frame().height as i32
        {
            // A click on a question's buttons or the search bar's options; nothing else.
            if msg == WM_LBUTTONUP {
                let w = o.renderer.metrics().width.max(1) as i32;
                let col = u16::try_from((x - o.renderer.padding() as i32).div_euclid(w)).ok();
                if let Some(q) = &self.ask {
                    if let Some(a) = col.and_then(|c| q.hit(c)) {
                        self.answer(a);
                    }
                } else if let Some(t) = col.and_then(|c| self.find.as_ref()?.hit(c)) {
                    if let Some(f) = &mut self.find {
                        f.toggle(t);
                    }
                    self.find_changed();
                }
            }
            return;
        }
        if matches!(msg, WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN) {
            // A click cancels a pending shortcut sequence (§5.3) and ends a rename field.
            self.matcher.cancel();
            if self.rename.take().is_some() {
                self.rebuild_bar();
            }
        }
        if !self.model.phase.shows_pane() {
            return;
        }
        let live = matches!(self.model.phase, Phase::Attached { .. });
        let dragging = self.drag.is_some();
        let Some((col, row)) = self.cell_at(x, y, dragging) else {
            return;
        };
        let action = match msg {
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => MouseAction::Press(MouseButton::Left),
            WM_LBUTTONUP => MouseAction::Release(MouseButton::Left),
            WM_RBUTTONDOWN => MouseAction::Press(MouseButton::Right),
            WM_RBUTTONUP => MouseAction::Release(MouseButton::Right),
            WM_MBUTTONDOWN => MouseAction::Press(MouseButton::Middle),
            WM_MBUTTONUP => MouseAction::Release(MouseButton::Middle),
            _ => MouseAction::Move,
        };
        // Step 19: Ctrl+click opens a link, Ctrl+right click opens the pane menu (a program that
        // tracks the mouse gets plain Ctrl+clicks; Shift+Ctrl reaches the window, as in xterm).
        let tracking =
            live && self.pane.screen().modes().mouse != subak_term::screen::MouseTracking::Off;
        if m.ctrl && !dragging && (!tracking || m.shift) {
            match msg {
                WM_LBUTTONDOWN if self.link_click(col, row) => return,
                WM_RBUTTONUP => {
                    self.menu_link = links::link_at(&self.pane, col, row);
                    self.open_pane_menu(Some((x, y)));
                    return;
                }
                WM_RBUTTONDOWN => return,
                _ => {}
            }
        }
        if live && !dragging {
            let c = self.cells();
            let ev = MouseEvent {
                action,
                col,
                row,
                x_px: (x - c.pad as i32).max(0) as u32,
                y_px: (y - c.pad as i32).max(0) as u32,
                mods: m,
            };
            match self.pane.mouse(&ev) {
                MouseOutcome::Send(b) => {
                    self.input(&b);
                    return;
                }
                MouseOutcome::Scrolled => {
                    self.redraw();
                    return;
                }
                MouseOutcome::Local => {}
            }
        }
        // The window's own selection (Shift always gets here, as in xterm).
        match msg {
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
                let now = Instant::now();
                // SAFETY: a plain system setting.
                let dbl = u128::from(unsafe { GetDoubleClickTime() });
                let count = match self.clicks {
                    (Some(t), at, n)
                        if at == (col, row) && now.duration_since(t).as_millis() <= dbl =>
                    {
                        n + 1
                    }
                    _ => 1,
                };
                let count = if msg == WM_LBUTTONDBLCLK {
                    count.max(2)
                } else {
                    count
                };
                self.clicks = (Some(now), (col, row), if count >= 3 { 0 } else { count });
                let mode = if m.alt {
                    SelectMode::Block
                } else {
                    match count {
                        1 => SelectMode::Cell,
                        2 => SelectMode::Word,
                        _ => SelectMode::Line,
                    }
                };
                self.pane.select(col, row, mode);
                self.drag = Some(Drag {
                    from: (col, row),
                    moved: count > 1,
                });
                // SAFETY: captures the mouse for our own window during the drag.
                unsafe { SetCapture(self.hwnd) };
                self.redraw();
            }
            WM_MOUSEMOVE => {
                if let Some(d) = &mut self.drag {
                    if (col, row) != d.from {
                        d.moved = true;
                    }
                    self.pane.extend_selection(col, row);
                    self.redraw();
                }
            }
            WM_LBUTTONUP => {
                if let Some(d) = self.drag.take() {
                    // SAFETY: ends our own capture.
                    unsafe { ReleaseCapture() };
                    if !d.moved {
                        self.pane.clear_selection();
                    } else if self.settings.terminal.copy_on_select && self.pane.has_selection() {
                        // Step 20, off by default: the selection is copied when the button is
                        // released, and stays selected (PuTTY style).
                        self.copy_keep();
                    }
                    self.redraw();
                }
            }
            WM_RBUTTONDOWN => match self.settings.terminal.right_click {
                // Windows Terminal's right click: copy a selection, else paste.
                RightClick::CopyPaste => {
                    if self.pane.has_selection() {
                        self.copy(false);
                    } else {
                        self.paste(false);
                    }
                }
                RightClick::Paste => self.paste(false),
                RightClick::Menu => {
                    self.menu_link = links::link_at(&self.pane, col, row);
                    self.open_pane_menu(Some((x, y)));
                }
                RightClick::None => {}
            },
            WM_MBUTTONDOWN if self.settings.terminal.middle_click == MiddleClick::Paste => {
                self.paste(false);
            }
            _ => {}
        }
    }

    /// `WM_MOUSEWHEEL`: `delta` in 1/120 notches at client point `pt`.
    fn on_wheel(&mut self, delta: i32, pt: POINT, m: Mods) {
        self.wheel += delta;
        while self.wheel.abs() >= 120 {
            let up = self.wheel > 0;
            self.wheel -= if up { 120 } else { -120 };
            if !self.model.phase.shows_pane() {
                continue;
            }
            let (col, row) = self.cell_at(pt.x, pt.y, true).unwrap_or((0, 0));
            if matches!(self.model.phase, Phase::Attached { .. }) {
                let ev = MouseEvent {
                    action: MouseAction::Press(if up {
                        MouseButton::WheelUp
                    } else {
                        MouseButton::WheelDown
                    }),
                    col,
                    row,
                    x_px: 0,
                    y_px: 0,
                    mods: m,
                };
                match self.pane.mouse(&ev) {
                    MouseOutcome::Send(b) => self.input(&b),
                    MouseOutcome::Scrolled | MouseOutcome::Local => {}
                }
            } else {
                let n = isize::try_from(subak_term::view::WHEEL_LINES).unwrap_or(3);
                self.pane.scroll_lines(if up { n } else { -n });
            }
        }
        self.redraw();
    }

    // -----------------------------------------------------------------------------------------
    // Test commands (WM_COPYDATA; test hooks only).

    fn dump(&self) -> Value {
        let surface = if self.model.phase.shows_pane() {
            &self.pane
        } else {
            &self.state_view
        };
        let screen: Vec<String> = (0..surface.size().1)
            .map(|r| surface.row_at(r).text().trim_end().to_string())
            .collect();
        let m = self.renderer.metrics();
        let mut dump = json!({
            "event": "dump",
            "phase": self.model.phase.name(),
            "terminal": self.model.terminal().map(|t| json!({"id": t.id, "name": t.name, "pid": t.pid})),
            "stream": self.model.input_stream(),
            "grid": [self.grid.0, self.grid.1],
            "pane_size": [self.pane.size().0, self.pane.size().1],
            "client": [self.client.0, self.client.1],
            "dpi": self.dpi,
            "cell": [m.width, m.height],
            "padding": self.renderer.padding(),
            "font_size": self.settings.appearance.font_size,
            "focused": self.focused,
            "minimized": self.minimized,
            "maximized": self.maximized,
            "title": self.title,
            "theme": self.theme().name,
            "rasterizer": self.renderer.rasterizer_name(),
            "screen": screen,
            "banner": self.banner.as_ref().map(|(b, _)| b.text.clone()),
            "composition": self.composition,
            "selection": self.pane.selected_text(),
            "clipboard": self.fake_clipboard().and_then(|c| c.get(clip::TEXT).map(String::from)),
            "scroll_offset": self.pane.scroll_offset(),
            "frames": self.frames,
            "bytes": self.bytes,
            "connected": self.model.connected(),
            "prefs_dir": self.prefs_dir.as_ref().map(|d| d.display().to_string()),
            "notice": self.notice,
            "memory": {"renderer": self.renderer.memory_bytes()},
            "shortcuts": self.matcher.bindings().into_iter()
                .map(|(a, k, scope)| json!({"action": a, "keys": k, "scope": scope}))
                .collect::<Vec<_>>(),
            "hotkeys": self.hotkeys_json(),
            "tabs": self.tabs_json()["tabs"],
            "active": self.model.active(),
            "hidden": self.tabs_json()["hidden"],
            "bar": self.bar_json(),
            "renaming": self.rename.as_ref().map(|r| json!({"id": r.id, "text": r.text, "cursor": r.cursor})),
            "confirm": self.confirm_json(),
            "banner_text": self.banner_text(),
        });
        if let (Some(d), Value::Object(more)) = (dump.as_object_mut(), self.step19_json()) {
            d.extend(more);
        }
        if let (Some(d), Value::Object(more)) = (dump.as_object_mut(), self.step20_json()) {
            d.extend(more);
        }
        dump
    }

    /// The dump's Step 20 state: the settings in use, the theme, the font, opacity, motion and
    /// the hovered tab.
    fn step20_json(&self) -> Value {
        let a = &self.settings.appearance;
        let t = &self.settings.terminal;
        let mut alpha = 0u8;
        let mut flags = 0u32;
        // SAFETY: plain queries of our own window.
        let (layered, icon) = unsafe {
            let layered = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE) as u32 & WS_EX_LAYERED != 0;
            if layered {
                GetLayeredWindowAttributes(self.hwnd, null_mut(), &raw mut alpha, &raw mut flags);
            }
            (
                layered,
                SendMessageW(self.hwnd, WM_GETICON, ICON_SMALL as usize, 0) != 0,
            )
        };
        let m = self.renderer.metrics();
        json!({
            "settings": {
                "state": self.cfg.state().as_str(),
                "unsaved": self.cfg_unsaved,
                "theme": a.theme.as_str(),
                "font_family": a.font_family,
                "line_height": a.line_height,
                "padding": a.padding,
                "opacity": a.opacity,
                "motion": a.motion.as_str(),
                "bold_is_bright": a.bold_is_bright,
                "copy_on_select": t.copy_on_select,
                "smart_ctrl_c": t.smart_ctrl_c,
                "right_click": t.right_click.as_str(),
                "middle_click": t.middle_click.as_str(),
                "word_separators": self.pane.word_delimiters(),
                "default_profile": self.settings.default_profile().map(|p| p.name.clone()),
                "profiles": self.settings.profiles.iter().map(|p| p.name.clone()).collect::<Vec<_>>(),
            },
            "theme_colors": {
                "background": format!("{:06X}", self.theme().background.to_u32()),
                "foreground": format!("{:06X}", self.theme().foreground.to_u32()),
                "chrome": self.chrome().name,
            },
            "face": self.face,
            "cell_height_ratio": f64::from(m.height) / f64::from(a.font_size * self.scale()),
            "opacity_applied": if layered { u32::from(alpha) } else { 255 },
            "motion": {
                "enabled": self.motion.enabled,
                "system": self.system_animations,
                "active": self.motion.active(),
            },
            "hover": self.hover,
            "icon": icon,
            "toast": self.toast.is_some(),
        })
    }

    /// The dump's Step 19 state: the fake clipboard's formats, what was opened, the search
    /// bar, the highlights, mark mode and an open question.
    fn step19_json(&self) -> Value {
        json!({
            "clipboard_formats": self.fake_clipboard().map(|c| c.formats.iter()
                .map(|(n, v)| json!([n, v])).collect::<Vec<_>>()),
            "opened": self.fake_opener().map(|o| o.opened.iter()
                .map(|(verb, t)| json!({"verb": verb, "target": t})).collect::<Vec<_>>()),
            "save_dialog": self.save_picker.asked,
            "find": self.find.as_ref().map(|f| json!({
                "query": f.text, "cursor": f.cursor, "regex": f.regex,
                "case_sensitive": f.case_sensitive, "whole_word": f.whole_word,
                "matches": f.matches().len(), "current": f.current.map(|c| c.0),
                "status": f.status(),
                "current_line": f.current.map(|c| c.1.start.line),
                "lines_searched": f.search.as_ref().map(|s| s.lines_searched),
                "elapsed_us": f.search.as_ref().map(|s| s.elapsed.as_micros() as u64),
            })),
            "highlights": self.pane.highlights().len(),
            "marking": self.pane.marking(),
            "ask": self.ask.as_ref().map(|q| json!({
                "words": q.words(), "focus": format!("{:?}", q.focused()),
                "answers": q.answers().iter().map(|a| format!("{a:?}")).collect::<Vec<_>>(),
                "spans": q.spans.iter().map(|(a, b, x)| json!([a, b, format!("{x:?}")]))
                    .collect::<Vec<_>>(),
                "banner": self.banner.as_ref().map(|(_, o)| json!({
                    "cell": o.renderer.metrics().width, "pad": o.renderer.padding(),
                    "height": o.renderer.frame().height})),
            })),
        })
    }

    fn fake_clipboard(&self) -> Option<&FakeClipboard> {
        self.clipboard.fake()
    }

    fn fake_opener(&self) -> Option<&FakeOpener> {
        self.opener.fake()
    }

    fn hotkeys_json(&self) -> Value {
        json!({
            "state": self.matcher.config_state().as_str(),
            "problem": self.matcher.problem,
            "off": self.matcher.off(),
            "path": self.matcher.path().display().to_string(),
            "pending": self.matcher.pending_keys(),
        })
    }

    fn bar_json(&self) -> Value {
        let spans: Vec<Value> = self
            .bar
            .layout
            .spans
            .iter()
            .map(|(a, b, h)| {
                let what = match h {
                    Hit::Tab(i) => format!("tab:{i}"),
                    Hit::Close(i) => format!("close:{i}"),
                    Hit::New => "new".into(),
                    Hit::Left => "left".into(),
                    Hit::Right => "right".into(),
                };
                json!([a, b, what])
            })
            .collect();
        let cell = self
            .bar
            .renderer
            .as_ref()
            .map(|r| [r.metrics().width, r.metrics().height]);
        json!({
            "text": self.bar_text(),
            "height": self.bar.height,
            "pad_x": self.bar.pad_x,
            "cell": cell,
            "spans": spans,
        })
    }

    fn confirm_json(&self) -> Value {
        let Some(c) = &self.confirm else {
            return Value::Null;
        };
        let name = |ch: Choice| if ch == Choice::End { "end" } else { "cancel" };
        let spans: Vec<Value> = c
            .spans
            .iter()
            .map(|(a, b, ch)| json!([a, b, name(*ch)]))
            .collect();
        let banner = self.banner.as_ref().map(|(_, o)| {
            json!({"cell": o.renderer.metrics().width, "pad": o.renderer.padding(),
                "height": o.renderer.frame().height})
        });
        json!({
            "id": c.info.id, "name": c.info.name, "program": c.info.program,
            "processes": c.info.processes, "focus": name(c.focus), "spans": spans,
            "banner": banner,
        })
    }

    /// The tab bar's text as drawn.
    fn bar_text(&self) -> String {
        let mut v = TermView::new(
            u16::try_from(
                self.bar
                    .layout
                    .spans
                    .last()
                    .map_or(1, |s| usize::from(s.1).max(1)),
            )
            .unwrap_or(1),
            1,
            0,
        );
        v.feed(&self.bar.layout.vt);
        v.row_at(0).text().trim_end().to_string()
    }

    /// The banner's text as drawn (the confirmation's buttons included).
    fn banner_text(&self) -> Option<String> {
        let c = self.confirm.as_ref()?;
        let (vt, _) = screens::confirm_vt(&c.info, c.focus, 400);
        let mut v = TermView::new(400, 1, 0);
        v.feed(&vt);
        Some(v.row_at(0).text().trim().to_string())
    }

    fn test_command(&mut self, cmd: &Value) {
        let mods_of = |v: &Value| Mods {
            shift: v["shift"] == true,
            ctrl: v["ctrl"] == true,
            alt: v["alt"] == true,
        };
        match cmd["cmd"].as_str().unwrap_or("") {
            "dump" => {}
            "type" => {
                for c in cmd["text"].as_str().unwrap_or("").chars() {
                    self.type_char(c, false, Mods::NONE);
                }
            }
            "key" => {
                let code = cmd["vk"]
                    .as_u64()
                    .and_then(|v| u32::try_from(v).ok())
                    .unwrap_or(0);
                if cmd["up"] == true {
                    self.key_up(code);
                } else {
                    let m = mods_of(cmd);
                    let p = Press {
                        vk: code,
                        scan: cmd["scan"]
                            .as_u64()
                            .and_then(|v| u32::try_from(v).ok())
                            .unwrap_or(0),
                        extended: false,
                        mods: keys::contract_mods(m),
                        ch: cmd["char"].as_str().and_then(|c| c.chars().next()),
                        repeat: cmd["repeat"] == true,
                    };
                    let handled = self.key_down(&p, m);
                    // As TranslateMessage would: an unhandled key with a character types it
                    // (WM_SYSCHAR with Alt alone; AltGr, Ctrl+Alt, gives a WM_CHAR).
                    if !handled && let Some(c) = cmd["char"].as_str().and_then(|c| c.chars().next())
                    {
                        self.type_char(c, m.alt && !m.ctrl, m);
                    }
                }
            }
            "menu" => {
                let tab = cmd["tab"].as_u64().and_then(|t| usize::try_from(t).ok());
                match cmd["id"].as_u64().and_then(|v| u32::try_from(v).ok()) {
                    Some(id) => self.menu_command(id, tab),
                    None => self.open_menu(tab, None),
                }
            }
            "paste" => {
                if let (Some(t), Some(c)) = (cmd["text"].as_str(), self.clipboard.as_fake()) {
                    c.set(vec![(clip::TEXT.to_string(), t.to_string())]);
                }
                self.paste(cmd["one_line"] == true);
            }
            // What another program put on the (fake) clipboard: `[[format, content], …]`.
            "clipboard" => {
                let formats: Vec<(String, String)> = cmd["formats"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|f| {
                                Some((f[0].as_str()?.to_string(), f[1].as_str()?.to_string()))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if let Some(c) = self.clipboard.as_fake() {
                    c.set(formats);
                }
            }
            // The path the (fake) save dialog answers with; null: the user cancels it.
            "save_dialog" => {
                self.save_picker.answer = cmd["path"].as_str().map(PathBuf::from);
            }
            "select" => {
                let p = |k: &str| {
                    (
                        cmd[k][0]
                            .as_u64()
                            .and_then(|v| u16::try_from(v).ok())
                            .unwrap_or(0),
                        cmd[k][1]
                            .as_u64()
                            .and_then(|v| u16::try_from(v).ok())
                            .unwrap_or(0),
                    )
                };
                let mode = match cmd["mode"].as_str() {
                    Some("word") => SelectMode::Word,
                    Some("line") => SelectMode::Line,
                    Some("block") => SelectMode::Block,
                    _ => SelectMode::Cell,
                };
                let (a, b) = (p("from"), p("to"));
                self.pane.select(a.0, a.1, mode);
                self.pane.extend_selection(b.0, b.1);
                self.redraw();
            }
            "dpi" => {
                let dpi = cmd["dpi"]
                    .as_u64()
                    .and_then(|v| u32::try_from(v).ok())
                    .unwrap_or(96);
                self.on_dpi(dpi, None);
            }
            "size" => {
                let w = cmd["width"]
                    .as_u64()
                    .and_then(|v| u32::try_from(v).ok())
                    .unwrap_or(800);
                let h = cmd["height"]
                    .as_u64()
                    .and_then(|v| u32::try_from(v).ok())
                    .unwrap_or(600);
                let outer = outer_for(w, h, self.dpi, true);
                self.after.push(After::Place(None, outer));
            }
            "focus" => self.on_focus(cmd["on"] == true),
            "ime" => {
                let s = |k: &str| cmd[k].as_str().map(String::from);
                self.on_composition(s("result"), s("composition"));
            }
            "zoom" => {
                let step = cmd["step"].as_i64().unwrap_or(0);
                self.set_font_size(Prefs::zoomed(
                    self.settings.appearance.font_size,
                    step.signum() as i32,
                ));
            }
            // Step 20: what Windows reports, simulated (High Contrast, light mode, animations).
            "system" => {
                if let Some(hc) = cmd["high_contrast"].as_bool() {
                    self.high_contrast = hc;
                }
                if let Some(l) = cmd["light_mode"].as_bool() {
                    self.light_mode = l;
                }
                if let Some(an) = cmd["animations"].as_bool() {
                    self.system_animations = an;
                    self.motion.enabled = self
                        .settings
                        .appearance
                        .motion
                        .animates(self.system_animations);
                }
                self.set_theme();
            }
            "wheel" => {
                // Positive notches scroll back (up), as a real wheel turned away from the user.
                let notches = cmd["notches"].as_i64().unwrap_or(1).clamp(-100, 100);
                let c = self.cells();
                let pt = POINT {
                    x: c.pad as i32 + 1,
                    y: c.pad as i32 + 1,
                };
                self.on_wheel(i32::try_from(notches * 120).unwrap_or(0), pt, mods_of(cmd));
            }
            "shot" => {
                if let Some(path) = cmd["path"].as_str() {
                    let f = self.composed();
                    let ok = std::fs::write(path, f.to_bmp()).is_ok();
                    self.event(&json!({"event": "shot", "path": path, "ok": ok,
                        "width": f.width, "height": f.height, "hash": format!("{:016x}", f.hash())}));
                }
                return;
            }
            "close" => {
                self.close();
                return;
            }
            other => {
                self.event(&json!({"event": "error", "message": format!("unknown test command `{other}`")}));
                return;
            }
        }
        self.event(&self.dump());
    }

    fn close(&mut self) {
        self.leave_pane();
        let fx = self.model.on_close();
        self.apply(fx);
        self.save_prefs();
        self.event(&json!({"event": "closing"}));
        self.after.push(After::Destroy);
    }
}

// ---------------------------------------------------------------------------------------------
// The system clipboard, link opener and save dialog (never used by the test hooks, which use the
// fakes of `clip`).

/// The Windows clipboard, opened by the window for each read or write.
#[derive(Debug)]
struct SystemClipboard {
    hwnd: HWND,
}

impl Clipboard for SystemClipboard {
    fn read_text(&mut self) -> Result<Option<String>, String> {
        // SAFETY: the clipboard is opened by our window and closed on every path; the data is
        // read while its handle is locked.
        unsafe {
            if OpenClipboard(self.hwnd) == 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            let h = GetClipboardData(u32::from(CF_UNICODETEXT));
            let text = if h.is_null() {
                None
            } else {
                let p = GlobalLock(h).cast::<u16>();
                if p.is_null() {
                    None
                } else {
                    let mut n = 0usize;
                    while *p.add(n) != 0 {
                        n += 1;
                    }
                    let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
                    GlobalUnlock(h);
                    Some(s)
                }
            };
            CloseClipboard();
            Ok(text)
        }
    }

    fn write(&mut self, text: &str, html: Option<&str>) -> Result<(), String> {
        /// A movable global block holding `bytes`, or null.
        unsafe fn block(bytes: &[u8]) -> HANDLE {
            // SAFETY: a movable global block of the right size, filled while locked.
            unsafe {
                let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len());
                if h.is_null() {
                    return null_mut();
                }
                let p = GlobalLock(h).cast::<u8>();
                if p.is_null() {
                    return null_mut();
                }
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
                GlobalUnlock(h);
                h as HANDLE
            }
        }
        let units: Vec<u8> = text
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        // SAFETY: the clipboard is opened by our window and closed on every path; ownership of
        // each block passes to the clipboard when SetClipboardData succeeds.
        unsafe {
            if OpenClipboard(self.hwnd) == 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            EmptyClipboard();
            let ok = !SetClipboardData(u32::from(CF_UNICODETEXT), block(&units)).is_null();
            if let Some(h) = html {
                let name = wide("HTML Format");
                let fmt = RegisterClipboardFormatW(name.as_ptr());
                if fmt != 0 {
                    let mut bytes = h.as_bytes().to_vec();
                    bytes.push(0);
                    SetClipboardData(fmt, block(&bytes));
                }
            }
            CloseClipboard();
            if ok {
                Ok(())
            } else {
                Err("the clipboard did not take the text".into())
            }
        }
    }
}

/// The OS "open" handler: `ShellExecuteW` with the verb `open` and the validated target as the
/// file — no command line, no shell.
#[derive(Debug)]
struct ShellOpener {
    hwnd: HWND,
}

impl Opener for ShellOpener {
    fn open(&mut self, target: &str) -> Result<(), String> {
        let verb = wide("open");
        let file = wide(target);
        // SAFETY: NUL-terminated wide strings that outlive the call; no parameters or folder.
        let r = unsafe {
            ShellExecuteW(
                self.hwnd,
                verb.as_ptr(),
                file.as_ptr(),
                null(),
                null(),
                SW_SHOWNORMAL,
            )
        };
        if r as isize > 32 {
            Ok(())
        } else {
            Err(format!("ShellExecute error {}", r as isize))
        }
    }

    fn edit(&mut self, path: &std::path::Path) -> Result<(), String> {
        let verb = wide("open");
        let program = wide("notepad.exe");
        let args = wide(&format!("\"{}\"", path.display()));
        // SAFETY: NUL-terminated wide strings that outlive the call; Notepad, the file as its one
        // quoted argument (no shell command line).
        let r = unsafe {
            ShellExecuteW(
                self.hwnd,
                verb.as_ptr(),
                program.as_ptr(),
                args.as_ptr(),
                null(),
                SW_SHOWNORMAL,
            )
        };
        if r as isize > 32 {
            Ok(())
        } else {
            Err(format!("ShellExecute error {}", r as isize))
        }
    }
}

/// The save dialog (modal: run with the window's state free). The chosen path, or `None`.
fn save_dialog(hwnd: HWND, suggested: &str) -> Option<PathBuf> {
    let mut buf = vec![0u16; 1024];
    for (i, u) in suggested.encode_utf16().take(1000).enumerate() {
        buf[i] = u;
    }
    let filter: Vec<u16> = "Text files (*.txt)\0*.txt\0All files (*.*)\0*.*\0\0"
        .encode_utf16()
        .collect();
    let ext = wide("txt");
    let title = wide("Save the output");
    let mut ofn = OPENFILENAMEW {
        lStructSize: size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: hwnd,
        lpstrFilter: filter.as_ptr(),
        lpstrFile: buf.as_mut_ptr(),
        nMaxFile: buf.len() as u32,
        lpstrTitle: title.as_ptr(),
        lpstrDefExt: ext.as_ptr(),
        Flags: OFN_EXPLORER | OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        ..OPENFILENAMEW::default()
    };
    // SAFETY: every pointer in `ofn` refers to a buffer that outlives the modal call.
    let ok = unsafe { GetSaveFileNameW(&raw mut ofn) } != 0;
    if !ok {
        return None;
    }
    let n = buf.iter().position(|&u| u == 0).unwrap_or(buf.len());
    Some(PathBuf::from(String::from_utf16_lossy(&buf[..n])))
}

// ---------------------------------------------------------------------------------------------
// The window procedure.

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        // SAFETY: default handling (the state does not exist yet).
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    }
    let mut after = Vec::new();
    let handled = APP.with(|cell| {
        let Ok(mut slot) = cell.try_borrow_mut() else {
            return None;
        };
        let app = slot.as_mut()?;
        let r = handle(app, hwnd, msg, wp, lp);
        after = std::mem::take(&mut app.after);
        r
    });
    run_after(hwnd, after);
    // SAFETY: default handling of everything not handled above.
    handled.unwrap_or_else(|| unsafe { DefWindowProcW(hwnd, msg, wp, lp) })
}

/// Runs the queued [`After`] actions (the state must not be borrowed).
fn run_after(hwnd: HWND, after: Vec<After>) {
    for a in after {
        // SAFETY: Win32 calls on our own window, made with the state free (they send messages).
        unsafe {
            match a {
                After::Place(pos, (w, h)) => {
                    let (x, y, flags) = match pos {
                        Some((x, y)) => (x, y, SWP_NOZORDER | SWP_NOACTIVATE),
                        None => (0, 0, SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOMOVE),
                    };
                    SetWindowPos(hwnd, null_mut(), x, y, w, h, flags);
                }
                After::Title(t) => {
                    let t = wide(&t);
                    SetWindowTextW(hwnd, t.as_ptr());
                }
                After::Destroy => {
                    DestroyWindow(hwnd);
                }
                After::Foreground => {
                    if IsIconic(hwnd) != 0 {
                        ShowWindow(hwnd, SW_RESTORE);
                    }
                    SetForegroundWindow(hwnd);
                }
                After::Opacity(alpha) => {
                    let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
                    if alpha == 255 {
                        if ex & WS_EX_LAYERED != 0 {
                            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, (ex & !WS_EX_LAYERED) as isize);
                        }
                    } else {
                        if ex & WS_EX_LAYERED == 0 {
                            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, (ex | WS_EX_LAYERED) as isize);
                        }
                        SetLayeredWindowAttributes(hwnd, 0, alpha, LWA_ALPHA);
                    }
                }
                After::Caption { dark, colors } => set_caption(hwnd, dark, colors),
                After::Menu { items, at, tab } => {
                    let Some(menu) = build_menu(&items) else {
                        continue;
                    };
                    let mut pt = POINT { x: at.0, y: at.1 };
                    ClientToScreen(hwnd, &raw mut pt);
                    // A modal loop of its own: the window's state is free meanwhile, so network
                    // messages keep being handled.
                    let cmd = TrackPopupMenuEx(
                        menu,
                        TPM_RETURNCMD | TPM_RIGHTBUTTON,
                        pt.x,
                        pt.y,
                        hwnd,
                        null(),
                    );
                    DestroyMenu(menu);
                    if cmd > 0 {
                        PostMessageW(hwnd, WM_MENU, cmd as usize, tab);
                    }
                }
                After::SaveDialog { suggested, hide } => {
                    // Its own modal loop, with the state free; the answer goes back to it.
                    let path = save_dialog(hwnd, &suggested);
                    APP.with(|cell| {
                        if let Ok(mut slot) = cell.try_borrow_mut()
                            && let Some(app) = slot.as_mut()
                        {
                            app.save_export(path, hide);
                        }
                    });
                    flush_after(hwnd);
                }
            }
        }
    }
}

/// A native popup menu of `items` (submenus included); `None` if Windows has no menu for us.
fn build_menu(items: &[MenuItem]) -> Option<windows_sys::Win32::UI::WindowsAndMessaging::HMENU> {
    // SAFETY: a new popup menu owned by the caller (destroyed after use with its submenus);
    // every label is a NUL-terminated wide string that outlives its call.
    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return None;
        }
        for it in items {
            let w = wide(&it.label);
            if !it.items.is_empty() {
                if let Some(sub) = build_menu(&it.items) {
                    AppendMenuW(menu, MF_STRING | MF_POPUP, sub as usize, w.as_ptr());
                }
            } else if it.id == 0 && it.label.is_empty() {
                AppendMenuW(menu, MF_SEPARATOR, 0, null());
            } else {
                let mut flags = MF_STRING;
                if !it.enabled {
                    flags |= MF_GRAYED;
                }
                if it.checked {
                    flags |= MF_CHECKED;
                }
                AppendMenuW(menu, flags, it.id as usize, w.as_ptr());
            }
        }
        Some(menu)
    }
}

/// The caption in a theme's colours: dark or light mode, then caption, text and border colours
/// (Windows 11; Windows 10 takes only the mode).
fn set_caption(hwnd: HWND, dark: bool, colors: [Rgb; 3]) {
    // SAFETY: documented DWM attributes of our own window, each a 4-byte value.
    unsafe {
        let d: i32 = i32::from(dark);
        DwmSetWindowAttribute(hwnd, DWMWA_DARK, (&raw const d).cast(), 4);
        for (attr, c) in [DWMWA_CAPTION, DWMWA_TEXT, DWMWA_BORDER]
            .into_iter()
            .zip(colors)
        {
            let v = colorref(c);
            DwmSetWindowAttribute(hwnd, attr, (&raw const v).cast(), 4);
        }
    }
}

/// Subak's watermelon as the window's small and large icons (taskbar, Alt+Tab, caption), drawn
/// by the floating icon's art at the monitor's DPI (Step 20; it was the generic icon before).
fn set_window_icon(hwnd: HWND, dpi: u32) {
    use super::float::art::{HostState, IconState, Palette, render};
    let state = IconState {
        host: HostState::Connected,
        ..IconState::default()
    };
    for (which, logical) in [(ICON_SMALL, 16u32), (ICON_BIG, 32u32)] {
        let size = (logical * dpi / 96).max(logical);
        let pic = render(&state, size, &Palette::subak());
        let icon = super::float::win::hicon(&pic);
        if !icon.is_null() {
            // SAFETY: our own window; the icon stays alive for the window's life (a few KiB).
            unsafe { SendMessageW(hwnd, WM_SETICON, which as usize, icon as isize) };
        }
    }
}

/// The modifiers held, with their side when only one side is down (for side-specific bindings).
fn sided_mods(m: Mods) -> kim_hotkeys::keys::Mods {
    // SAFETY: reads this thread's keyboard state.
    let down = |k: u32| unsafe { GetKeyState(k as i32) } < 0;
    let side = |generic: bool, l: u32, r: u32| match (down(l), down(r)) {
        (true, false) => Side::Left,
        (false, true) => Side::Right,
        (true, true) => Side::Any,
        (false, false) if generic => Side::Any,
        _ => Side::None,
    };
    kim_hotkeys::keys::Mods {
        ctrl: side(m.ctrl, vk::LCONTROL, vk::RCONTROL),
        alt: side(m.alt, vk::LMENU, vk::RMENU),
        altgr: false,
        shift: side(m.shift, vk::LSHIFT, vk::RSHIFT),
        win: if down(vk::LWIN) || down(vk::RWIN) {
            Side::Any
        } else {
            Side::None
        },
    }
}

/// The character a key produces on the current layout with Shift as held (for `char:` steps),
/// without changing the keyboard's dead-key state (`ToUnicode` flag 4, Windows 10 1607+).
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

/// A key press, before `TranslateMessage` turns it into a character (as accelerators are): a
/// key the window handles (a shortcut, a cursor or function key for the program, a key of the
/// confirmation or the rename field)
/// never produces a `WM_CHAR`, so nothing has to be taken back out of the queue. Returns whether
/// it was handled. Alt+F4 always goes on to the window (close = detach).
fn pre_translate(msg: &MSG) -> bool {
    let code = msg.wParam as u32;
    if matches!(msg.message, WM_KEYUP | WM_SYSKEYUP) {
        // Key releases keep their default handling; a `release` shortcut fires on them.
        APP.with(|cell| {
            if let Ok(mut slot) = cell.try_borrow_mut()
                && let Some(app) = slot.as_mut()
                && app.hwnd == msg.hwnd
            {
                app.key_up(code);
            }
        });
        flush_after(msg.hwnd);
        return false;
    }
    if !matches!(msg.message, WM_KEYDOWN | WM_SYSKEYDOWN) {
        return false;
    }
    if msg.message == WM_SYSKEYDOWN && code == vk::F4 {
        return false;
    }
    let lp = msg.lParam as usize;
    let handled = APP.with(|cell| {
        let Ok(mut slot) = cell.try_borrow_mut() else {
            return false;
        };
        slot.as_mut().is_some_and(|app| {
            if app.hwnd != msg.hwnd {
                return false;
            }
            let m = mods();
            let scan = ((lp >> 16) & 0xFF) as u32;
            let p = Press {
                vk: code,
                scan,
                extended: lp & (1 << 24) != 0,
                mods: sided_mods(m),
                ch: app
                    .matcher
                    .wants_chars()
                    .then(|| produced_char(code, scan, m.shift))
                    .flatten(),
                repeat: lp & (1 << 30) != 0,
            };
            app.key_down(&p, m)
        })
    });
    flush_after(msg.hwnd);
    handled
}

/// Takes the queued [`After`] actions out of the state and runs them.
fn flush_after(hwnd: HWND) {
    let after = APP.with_borrow_mut(|slot| {
        slot.as_mut()
            .map(|a| std::mem::take(&mut a.after))
            .unwrap_or_default()
    });
    run_after(hwnd, after);
}

/// One message with the state borrowed. `None` = default handling.
fn handle(app: &mut App, hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    match msg {
        WM_PAINT => {
            app.paint();
            Some(0)
        }
        WM_ERASEBKGND => Some(1),
        WM_NET => {
            app.on_net();
            Some(0)
        }
        WM_MENU => {
            let tab = usize::try_from(lp).ok();
            app.menu_command(wp as u32, tab);
            Some(0)
        }
        WM_SIZE => {
            let (w, h) = (u32::from(lp as u16), u32::from((lp >> 16) as u16));
            app.on_size(wp as u32, w, h);
            Some(0)
        }
        WM_EXITSIZEMOVE => {
            app.remember_normal();
            app.save_prefs();
            Some(0)
        }
        WM_GETMINMAXINFO => {
            let c = app.cells();
            let (w, h) = c.client_for(layout::MIN_GRID.0, layout::MIN_GRID.1);
            let (ow, oh) = outer_for(w, h + app.top(), app.dpi, app.opts.test);
            // SAFETY: `lp` points at the MINMAXINFO Windows passes with this message.
            unsafe {
                let mmi = &mut *(lp as *mut MINMAXINFO);
                mmi.ptMinTrackSize = POINT { x: ow, y: oh };
                if app.opts.test {
                    // The off-screen test window is not limited to the desktop's size, so a
                    // synthetic DPI change behaves the same on a small CI screen.
                    mmi.ptMaxTrackSize = POINT {
                        x: 16_000,
                        y: 16_000,
                    };
                }
            }
            Some(0)
        }
        WM_DPICHANGED => {
            // SAFETY: `lp` points at the suggested RECT Windows passes with this message.
            let r = unsafe { &*(lp as *const RECT) };
            app.on_dpi(
                (wp & 0xffff) as u32,
                Some(Rect {
                    left: r.left,
                    top: r.top,
                    right: r.right,
                    bottom: r.bottom,
                }),
            );
            Some(0)
        }
        WM_SETTINGCHANGE | WM_SYSCOLORCHANGE | WM_THEMECHANGED => {
            if app.opts.test {
                // The off-screen test window simulates what Windows reports (`system` command),
                // so the machine's own settings never change a test.
                return None;
            }
            let hc = high_contrast_on();
            let light = apps_use_light_theme();
            let anim = system_animations();
            if anim != app.system_animations {
                app.system_animations = anim;
                app.motion.enabled = app.settings.appearance.motion.animates(anim);
                if !app.motion.enabled {
                    app.motion.stop();
                }
            }
            if hc != app.high_contrast || light != app.light_mode || msg == WM_SYSCOLORCHANGE {
                app.high_contrast = hc;
                app.light_mode = light;
                app.set_theme();
            }
            None
        }
        // Key presses are dispatched in the message loop before TranslateMessage (see
        // `pre_translate`); what arrives here was not handled there.
        WM_SYSKEYDOWN if wp as u32 == vk::F10 => Some(0),
        WM_SYSKEYUP if matches!(wp as u32, vk::MENU | vk::F10) => {
            // A lone Alt or F10 would open the (absent) menu bar and swallow the next key.
            Some(0)
        }
        WM_SYSCOMMAND if (wp & 0xFFF0) as u32 == SC_KEYMENU && lp != b' ' as isize => Some(0),
        WM_CHAR => {
            app.on_char(wp as u16, false, mods());
            Some(0)
        }
        WM_SYSCHAR => {
            if wp == b' ' as usize {
                return None; // Alt+Space: the window menu
            }
            app.on_char(wp as u16, true, mods());
            Some(0)
        }
        WM_SETFOCUS | WM_KILLFOCUS => {
            app.on_focus(msg == WM_SETFOCUS);
            Some(0)
        }
        WM_IME_SETCONTEXT => {
            // SAFETY: default handling, without the IME's own composition window.
            Some(unsafe { DefWindowProcW(hwnd, msg, wp, lp & !ISC_SHOWUICOMPOSITIONWINDOW) })
        }
        WM_IME_STARTCOMPOSITION => {
            app.place_ime();
            Some(0)
        }
        WM_IME_COMPOSITION => {
            let flags = lp as u32;
            let result = (flags & GCS_RESULTSTR != 0)
                .then(|| app.ime_string(GCS_RESULTSTR))
                .flatten();
            let composing = (flags & GCS_COMPSTR != 0)
                .then(|| app.ime_string(GCS_COMPSTR))
                .flatten();
            app.on_composition(result, composing);
            Some(0)
        }
        WM_IME_ENDCOMPOSITION => {
            app.on_composition(None, None);
            Some(0)
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP
        | WM_MBUTTONDOWN | WM_MBUTTONUP | WM_MOUSEMOVE => {
            let (x, y) = lparam_xy(lp);
            app.on_mouse(msg, wp, x, y);
            Some(0)
        }
        WM_MOUSELEAVE => {
            app.leave_tracked = false;
            if app.hover.take().is_some() {
                app.event(&json!({"event": "hover", "tab": null}));
                app.rebuild_bar();
            }
            Some(0)
        }
        WM_MOUSEWHEEL => {
            let delta = i32::from((wp >> 16) as u16 as i16);
            let (sx, sy) = lparam_xy(lp);
            let mut pt = POINT { x: sx, y: sy };
            // SAFETY: converts a screen point for our own window.
            unsafe { ScreenToClient(hwnd, &raw mut pt) };
            app.on_wheel(delta, pt, App::mouse_mods(wp));
            Some(0)
        }
        WM_SETCURSOR if (lp & 0xffff) as u32 == HTCLIENT => {
            let mut pt = POINT::default();
            // SAFETY: the cursor position, converted for our own window.
            let over_bar = unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos(&raw mut pt);
                ScreenToClient(hwnd, &raw mut pt);
                pt.y < app.top() as i32
            };
            let id = if app.model.phase.shows_pane() && !over_bar {
                IDC_IBEAM
            } else {
                IDC_ARROW
            };
            // SAFETY: a shared system cursor.
            unsafe { SetCursor(LoadCursorW(null_mut(), id)) };
            Some(1)
        }
        WM_TIMER => {
            match wp {
                TIMER_RECONNECT => {
                    // SAFETY: our own one-shot timer.
                    unsafe { KillTimer(hwnd, TIMER_RECONNECT) };
                    app.connect(false);
                }
                TIMER_TEST_EXIT => app.close(),
                TIMER_SYNC => {
                    // SAFETY: our own one-shot timer.
                    unsafe { KillTimer(hwnd, TIMER_SYNC) };
                    app.update_title();
                    app.redraw();
                }
                TIMER_HOLD => {
                    // SAFETY: our own one-shot timer.
                    unsafe { KillTimer(hwnd, TIMER_HOLD) };
                    if let Some(a) = app.matcher.hold_elapsed() {
                        app.run(a);
                    }
                }
                TIMER_MOTION => app.motion_tick(),
                TIMER_TOAST => {
                    // SAFETY: our own one-shot timer.
                    unsafe { KillTimer(hwnd, TIMER_TOAST) };
                    // Only the toast itself hides: a notice that replaced it meanwhile stays.
                    if let Some(t) = app.toast.take()
                        && app.notice.as_deref() == Some(t.as_str())
                    {
                        app.notice = None;
                        app.rebuild_banner();
                    }
                }
                _ => {}
            }
            Some(0)
        }
        WM_COPYDATA => {
            let req =
                // SAFETY: the lParam of this WM_COPYDATA.
                unsafe { super::float::win::copydata(lp, super::float::COPYDATA_OPEN) };
            if let Some(req) = req {
                return Some(app.open_request(&req));
            }
            if !app.opts.test {
                return Some(0);
            }
            // SAFETY: `lp` points at the COPYDATASTRUCT of this message, valid during the call.
            let cds = unsafe { &*(lp as *const COPYDATASTRUCT) };
            if cds.dwData != COPYDATA_TEST || cds.lpData.is_null() {
                return Some(0);
            }
            // SAFETY: `cbData` bytes at `lpData`, as the sender described them.
            let bytes =
                unsafe { std::slice::from_raw_parts(cds.lpData.cast::<u8>(), cds.cbData as usize) };
            match serde_json::from_slice::<Value>(bytes) {
                Ok(cmd) => app.test_command(&cmd),
                Err(e) => app.event(&json!({"event": "error", "message": e.to_string()})),
            }
            Some(1)
        }
        WM_CLOSE => {
            app.close();
            Some(0)
        }
        WM_DESTROY => {
            app.event(&json!({"event": "exit"}));
            // SAFETY: ends this thread's message loop.
            unsafe { PostQuitMessage(0) };
            Some(0)
        }
        _ => None,
    }
}

/// Runs the window until it is closed.
pub fn run(o: Options) -> ExitCode {
    // SAFETY: opts this process into per-monitor DPI awareness v2 before any window exists.
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
    let prefs_dir = super::prefs::config_dir();
    let loaded = prefs_dir
        .as_deref()
        .map(Prefs::load)
        .unwrap_or(super::prefs::Loaded {
            prefs: Prefs::default(),
            problem: None,
            read_only: true,
        });
    let mut loaded = loaded;
    // Step 20: the settings, and the one-time move of gui.json version 1's text size and theme
    // into config.toml (where that file is silent; gui.json is backed up first).
    let mut cfg = prefs_dir
        .as_deref()
        .map(config::load)
        .unwrap_or_else(|| config::load(std::path::Path::new("")));
    let mut migration: Option<Value> = None;
    let mut settings = cfg.settings().clone();
    if let (Some(dir), Some(legacy)) = (prefs_dir.as_deref(), loaded.prefs.legacy) {
        if loaded.read_only {
            // Nothing can be rewritten: use the old values in memory.
            if let Some(f) = legacy.font_size {
                settings.appearance.font_size = f;
            }
            if let Some(t) = legacy.theme {
                settings.appearance.theme = t;
            }
        } else {
            let adopted = config::adopt(&cfg, legacy);
            let result = match adopted {
                Ok(edit) => {
                    let backup =
                        config::backup(dir, super::prefs::FILE, "v1").map_err(|e| e.to_string());
                    let wrote = match (&edit, &backup) {
                        (Some(e), Ok(_)) => config::write_atomic(&cfg.path, &e.text)
                            .map(|()| true)
                            .map_err(|e| e.to_string()),
                        (_, Err(e)) => Err(e.clone()),
                        (None, Ok(_)) => Ok(false),
                    };
                    wrote.and_then(|w| {
                        loaded.prefs.legacy = None;
                        loaded
                            .prefs
                            .save(dir)
                            .map(|()| (w, backup.ok().flatten()))
                            .map_err(|e| e.to_string())
                    })
                }
                Err(e) => Err(e.message()),
            };
            match result {
                Ok((wrote, backup)) => {
                    cfg = config::load(dir);
                    settings = cfg.settings().clone();
                    migration = Some(
                        json!({"event": "migrated", "from": 1, "to": super::prefs::VERSION,
                        "config_written": wrote, "backup": backup}),
                    );
                }
                Err(e) => {
                    // config.toml cannot take them (invalid or newer): keep gui.json version 1
                    // as it is and use its values in this window.
                    loaded.read_only = true;
                    if let Some(f) = legacy.font_size {
                        settings.appearance.font_size = f;
                    }
                    if let Some(t) = legacy.theme {
                        settings.appearance.theme = t;
                    }
                    loaded.problem = Some(format!(
                        "gui.json version 1 was not migrated ({e}); its text size and theme apply to this window"
                    ));
                }
            }
        }
    }
    let prefs = loaded.prefs.clone();
    let want = match (&o.target, &prefs.last_terminal) {
        (Some(t), _) => Want::Target(t.clone()),
        (None, Some(l)) => Want::Last {
            id: l.id.clone(),
            name: l.name.clone(),
        },
        (None, None) => Want::Any,
    };
    let test = o.test;
    let class = wide("SubakWindow");
    // SAFETY: registers our window class and creates one top-level window of this process. In
    // test mode it is a tool window far off every monitor, never activated.
    let hwnd = unsafe {
        let hinst = GetModuleHandleW(null());
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            hIcon: LoadIconW(null_mut(), IDI_APPLICATION),
            lpszClassName: class.as_ptr(),
            ..WNDCLASSEXW::default()
        };
        RegisterClassExW(&raw const wc);
        let title = wide("Subak");
        let restored = if test {
            None
        } else {
            prefs
                .window
                .as_ref()
                .and_then(|p| layout::restore(p, &monitors()))
        };
        let (x, y, w, h) = match (test, restored) {
            (true, _) => (OFFSCREEN, OFFSCREEN, 800, 600),
            (false, Some((r, _))) => (r.left, r.top, r.width(), r.height()),
            (false, None) => (CW_USEDEFAULT, CW_USEDEFAULT, 800, 600),
        };
        let hwnd = CreateWindowExW(
            ex_style(test),
            class.as_ptr(),
            title.as_ptr(),
            style(),
            x,
            y,
            w,
            h,
            null_mut(),
            null_mut(),
            hinst,
            null::<CREATESTRUCTW>().cast(),
        );
        if hwnd.is_null() {
            attach_parent_console();
            eprintln!(
                "subak-gui: error: cannot create the window: {} (io)",
                std::io::Error::last_os_error()
            );
            return ExitCode::from(1);
        }
        hwnd
    };
    // SAFETY: our own window.
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(48);
    let restored = !test
        && prefs
            .window
            .as_ref()
            .and_then(|p| layout::restore(p, &monitors()))
            .is_some();
    let maximized = !test && restored && prefs.window.is_some_and(|w| w.maximized);
    let hc = high_contrast_on();
    let animations = system_animations();
    let settings_notice = cfg.notice();
    let motion_on = settings.appearance.motion.animates(animations);
    let matcher = Matcher::load(
        prefs_dir.as_deref(),
        crate::hotkeys::off_reason(o.no_hotkeys),
    );
    let mut app = App {
        hwnd,
        opts: o,
        endpoint,
        model: Model::new(want, layout::DEFAULT_GRID).with_hidden(prefs.hidden_terminals.clone()),
        prefs,
        prefs_dir,
        prefs_read_only: loaded.read_only,
        notice: loaded.problem,
        pane: TermView::new(layout::DEFAULT_GRID.0, layout::DEFAULT_GRID.1, 0),
        state_view: TermView::new(1, 1, 0),
        renderer: Renderer::builtin(Theme::kim_dark(), RenderOptions::default()),
        banner: None,
        ime: None,
        composition: String::new(),
        dpi,
        client: (0, 0),
        grid: (0, 0),
        minimized: false,
        maximized: false,
        focused: false,
        high_contrast: hc,
        bg_brush: null_mut(),
        link: None,
        generation: 0,
        inbox: {
            let target = hwnd as isize;
            Inbox::new(Box::new(move || {
                // SAFETY: posting to our window is thread-safe; a closed window just drops it.
                unsafe { PostMessageW(target as HWND, WM_NET, 0, 0) };
            }))
        },
        high_surrogate: None,
        drag: None,
        clicks: (None, (0, 0), 0),
        wheel: 0,
        matcher,
        bar: TabBar {
            renderer: None,
            layout: tabs::layout(&[], None, None, 1, 0),
            strip: Frame::new(1, 1, Chrome::KIM_DARK.bg_raised),
            height: 0,
            pad_x: 0,
            first: 0,
        },
        rename: None,
        confirm: None,
        reported_tabs: Value::Null,
        normal: None,
        clipboard: if test {
            Box::new(FakeClipboard::default())
        } else {
            Box::new(SystemClipboard { hwnd })
        },
        opener: if test {
            Box::new(FakeOpener::default())
        } else {
            Box::new(ShellOpener { hwnd })
        },
        save_picker: FakeSavePicker::default(),
        find: None,
        find_caret: 0,
        find_stale: false,
        ask: None,
        export: None,
        menu_link: None,
        title: String::new(),
        frames: 0,
        bytes: 0,
        after: Vec::new(),
        settings,
        cfg,
        cfg_unsaved: false,
        light_mode: apps_use_light_theme(),
        system_animations: animations,
        motion: Transitions::new(motion_on),
        bar_active: None,
        hover: None,
        leave_tracked: false,
        toast: None,
        menu_choices: Vec::new(),
        fonts: None,
        face: String::new(),
    };
    if let Some(m) = &migration {
        app.event(m);
    }
    if app.notice.is_none() {
        app.notice = settings_notice;
    }
    app.renderer = make_term_renderer(
        app.theme(),
        RenderOptions {
            scale: layout::scale(dpi),
            ..app.term_opts()
        },
        &app.settings.appearance.font_family,
    );
    app.renderer.set_focused(false);
    app.face = app.renderer.face_for('A').family;
    app.pane
        .set_word_delimiters(&app.settings.terminal.word_separators);
    // The tab bar's height is known before the first size is chosen.
    app.client = (800, 600);
    app.rebuild_bar();
    if let Some(p) = app.matcher.problem.clone() {
        app.notice = Some(p);
    }
    // SAFETY: our own window's background brush (deleted when the theme changes).
    app.bg_brush = unsafe { CreateSolidBrush(colorref(app.theme().background)) };
    let outer = if restored {
        None
    } else if let Some(w) = app.prefs.window.filter(|_| test) {
        // The test window stays off-screen, but takes the saved size (persistence check).
        Some(layout::rescale((w.width, w.height), w.dpi, dpi))
    } else {
        let (cw, ch) = app
            .cells()
            .client_for(layout::DEFAULT_GRID.0, layout::DEFAULT_GRID.1);
        Some(outer_for(cw, ch + app.top(), dpi, test))
    };
    // Windows 10 20H1+ dark (or, for kim-light, light) title bar; the caption, text and border
    // colours apply on Windows 11 and are ignored on Windows 10. Step 20: the window's icon is
    // Subak's watermelon (the floating icon's picture), and its opacity follows the setting.
    let c = app.chrome();
    set_caption(
        hwnd,
        c.name != "kim-light",
        [c.bg_base, c.text_secondary, c.border_subtle],
    );
    set_window_icon(hwnd, dpi);
    app.apply_opacity();
    APP.with_borrow_mut(|slot| *slot = Some(app));
    // SAFETY: sizes and shows our own window (the state is set, so WM_SIZE is handled), then
    // runs this thread's message loop.
    unsafe {
        if let Some((w, h)) = outer {
            SetWindowPos(
                hwnd,
                null_mut(),
                0,
                0,
                w,
                h,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        let mut rc = RECT::default();
        GetClientRect(hwnd, &raw mut rc);
        APP.with_borrow_mut(|slot| {
            if let Some(app) = slot.as_mut() {
                app.on_size(0, rc.right as u32, rc.bottom as u32);
                app.update_title();
            }
        });
        flush_after(hwnd);
        ShowWindow(
            hwnd,
            if test {
                SW_SHOWNOACTIVATE
            } else if maximized {
                SW_SHOWMAXIMIZED
            } else {
                SW_SHOWNORMAL
            },
        );
        APP.with_borrow_mut(|slot| {
            if let Some(app) = slot.as_mut() {
                if test {
                    let secs = app.opts.test_max_seconds;
                    SetTimer(hwnd, TIMER_TEST_EXIT, secs.saturating_mul(1000), None);
                    app.event(&json!({
                        "event": "ready", "hwnd": hwnd as usize, "pid": std::process::id(),
                        "dpi": app.dpi, "grid": [app.grid.0, app.grid.1],
                        "client": [app.client.0, app.client.1],
                        "rasterizer": app.renderer.rasterizer_name(),
                    }));
                }
                app.rebuild_state();
                app.rebuild_banner();
                app.redraw();
                app.connect(true);
            }
        });
        flush_after(hwnd);
        let mut msg = MSG::default();
        while GetMessageW(&raw mut msg, null_mut(), 0, 0) > 0 {
            if pre_translate(&msg) {
                continue;
            }
            TranslateMessage(&raw const msg);
            DispatchMessageW(&raw const msg);
        }
    }
    APP.with_borrow_mut(|slot| {
        if let Some(app) = slot.take()
            && let Some(l) = &app.link
        {
            l.close();
        }
    });
    ExitCode::SUCCESS
}
