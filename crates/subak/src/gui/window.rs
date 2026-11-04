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
//! **Test hooks** (debug builds, or feature `test-hooks`; `--test-offscreen`): the window is
//! created far off every monitor (-20000, -20000) as a tool window (no taskbar button, not in
//! Alt+Tab) with `WS_EX_NOACTIVATE`, shown with `SW_SHOWNOACTIVATE`, so it never appears on the
//! desktop and never takes focus; it never minimizes or maximizes itself; it prints its state as
//! JSON lines on stdout, takes test commands as `WM_COPYDATA`, uses a private clipboard instead
//! of the system one, renders its own frames to BMP files on request (never a capture of the
//! screen), and closes itself after `--test-max-seconds`.

use super::Options;
use super::compose;
use super::keys::{self, Action, Matcher, Outcome, Press, Typed, vk};
use super::layout::{self, Cells, Rect};
use super::model::{Confirm, Effect, Frame as Data, Model, Phase, Want};
use super::net::{self, Inbox, Link, Msg};
use super::prefs::{LastTerminal, Placement, Prefs, ThemePref};
use super::screens::{self, Banner, Choice, Hints, tokens};
use super::tabs::{self, Hit};
use kim_hotkeys::keys::Side;
use serde_json::{Value, json};
use std::cell::RefCell;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::ptr::{null, null_mut};
use std::sync::Arc;
use std::time::Instant;
use subak_term::render::{Frame, RenderOptions, Renderer, Rgb, Theme};
use subak_term::{
    Key, Mods, MouseAction, MouseButton, MouseEvent, MouseOutcome, SelectMode, TermView,
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
    SetClipboardData,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows_sys::Win32::System::Ole::CF_UNICODETEXT;
use windows_sys::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
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
    GetDoubleClickTime, GetKeyState, ReleaseCapture, SetCapture, ToUnicode,
};
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

/// The network threads have queued something.
const WM_NET: u32 = WM_APP + 1;
/// The tab menu's choice (`wParam` the command, `lParam` the tab index or -1).
const WM_MENU: u32 = WM_APP + 2;
const TIMER_RECONNECT: usize = 1;
const TIMER_TEST_EXIT: usize = 2;
const TIMER_SYNC: usize = 3;
/// A `hold` shortcut's delay, armed only while its key is down.
const TIMER_HOLD: usize = 4;
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
/// `CMD_SHOW + k`: show the `k`-th hidden Terminal again.
const CMD_SHOW: u32 = 100;
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
        items: Vec<(u32, String, bool)>,
        at: (i32, i32),
        tab: isize,
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
    test_clipboard: String,
    title: String,
    frames: u64,
    bytes: u64,
    after: Vec<After>,
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
    fn theme(&self) -> Theme {
        let hc = match self.prefs.theme {
            ThemePref::Auto => self.high_contrast,
            ThemePref::KimDark => false,
            ThemePref::HighContrast => true,
        };
        if hc {
            let (w, t, h) = system_theme_colors();
            Theme::high_contrast(w, t, h)
        } else {
            Theme::kim_dark()
        }
    }

    fn hc_theme(&self) -> bool {
        self.theme().name == "high-contrast"
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
            v.feed(&if self.hc_theme() {
                screens::high_contrast(&vt)
            } else {
                vt
            });
        }
        self.state_view = v;
    }

    /// The tab bar's renderer theme: `bg.raised` with secondary text (the system colours in
    /// High Contrast).
    fn bar_theme(&self) -> Theme {
        let mut theme = self.theme();
        if !self.hc_theme() {
            theme.background = tokens::BG_RAISED;
            theme.foreground = tokens::TEXT_SECONDARY;
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
        let bar = tabs::layout(&items, active, edit.as_ref(), cols, self.bar.first);
        drop(items);
        let mut view = TermView::new(cols, 1, 0);
        if hc {
            view.feed(&screens::high_contrast(&bar.vt));
        } else {
            view.feed(&bar.vt);
        }
        r.render(&view);
        let style = if hc {
            compose::BarStyle {
                fill: theme.background,
                active_fill: None,
                marker: None,
                border: Some(theme.foreground),
            }
        } else {
            compose::BarStyle {
                fill: tokens::BG_RAISED,
                active_fill: Some(tokens::BG_BASE),
                marker: Some((tokens::ACCENT, layout::px(2.0, self.dpi).max(1) as u32)),
                border: Some(tokens::BORDER_SUBTLE),
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

    /// Rebuilds the banner: the busy confirmation, else a pending shortcut sequence, else a
    /// notice, else the phase's banner.
    fn rebuild_banner(&mut self) {
        let hc = self.hc_theme();
        let content: Option<(screens::Tone, Option<Banner>)> = if let Some(c) = &self.confirm {
            let _ = c;
            Some((screens::Tone::Danger, None))
        } else if let Some(keys) = self.matcher.pending_keys() {
            Some((
                screens::Tone::Info,
                Some(Banner {
                    tone: screens::Tone::Info,
                    text: format!("{keys} … waiting for the next key (Esc cancels)"),
                }),
            ))
        } else if let Some(n) = &self.notice {
            Some((
                screens::Tone::Info,
                Some(Banner {
                    tone: screens::Tone::Info,
                    text: format!("{n} (any key hides this)"),
                }),
            ))
        } else {
            screens::banner(&self.model.phase, &self.hints()).map(|b| (b.tone, Some(b)))
        };
        let Some((tone, text)) = content else {
            if self.banner.take().is_some() {
                self.invalidate_all();
            }
            return;
        };
        let mut theme = self.theme();
        if !hc {
            theme.background = tone.fill();
            theme.foreground = tokens::TEXT_PRIMARY;
        }
        let mut r = match self.banner.take() {
            Some((_, o))
                if o.renderer.options().font_size == self.prefs.font_size
                    && o.renderer.options().scale == self.scale() =>
            {
                o.renderer
            }
            _ => make_renderer(theme.clone(), self.prefs.font_size, self.scale(), 4.0),
        };
        r.set_theme(theme);
        let m = r.metrics();
        let cols = ((self.client.0.saturating_sub(2 * r.padding())) / m.width.max(1)).max(1);
        let cols = u16::try_from(cols).unwrap_or(u16::MAX);
        let mut view = TermView::new(cols, 1, 0);
        let (vt, b) = match (&text, &mut self.confirm) {
            (None, Some(c)) => {
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
            (Some(b), _) => (screens::banner_vt(b, cols), b.clone()),
            (None, None) => return,
        };
        view.feed(&if hc { screens::high_contrast(&vt) } else { vt });
        r.render(&view);
        self.banner = Some((b, Overlay { renderer: r }));
        self.invalidate_all();
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
            theme.background = tokens::BG_OVERLAY;
            theme.foreground = tokens::TEXT_PRIMARY;
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
            self.prefs.font_size
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
        let banner = self.banner.as_ref().map(|(b, o)| compose::BannerLayer {
            frame: o.renderer.frame(),
            fill: o.renderer.theme().background,
            marker: (!self.hc_theme())
                .then(|| (b.tone.color(), layout::px(3.0, self.dpi).max(1) as u32)),
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
                    let marker = layout::px(3.0, self.dpi).max(1) as u32;
                    fill(dc, 0, top, marker, bf.height, b.tone.color());
                    fill(
                        dc,
                        0,
                        top + bf.height.saturating_sub(1),
                        cw,
                        1,
                        tokens::BORDER_SUBTLE,
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

    fn set_font_size(&mut self, size: f32) {
        if (size - self.prefs.font_size).abs() < f32::EPSILON {
            return;
        }
        self.prefs.font_size = size;
        self.renderer.set_font_size(size);
        self.banner = None;
        self.ime = None;
        self.relayout();
        self.save_prefs();
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
        if !self.focused {
            let _ = v.focus(false);
        }
        self.pane = v;
        self.drag = None;
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
            if let Some(i) = self.model.active() {
                self.open_menu(Some(i), None);
            } else {
                self.open_menu(None, None);
            }
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
        if self.confirm.is_some() {
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
            Action::NewTerminal => {
                self.leave_pane();
                let fx = self.model.create();
                self.apply(fx);
            }
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
            Action::Copy => self.copy(),
            Action::Paste => self.paste(),
            Action::ZoomIn => self.set_font_size(Prefs::zoomed(self.prefs.font_size, 1)),
            Action::ZoomOut => self.set_font_size(Prefs::zoomed(self.prefs.font_size, -1)),
            Action::ZoomReset => self.set_font_size(Prefs::zoomed(self.prefs.font_size, 0)),
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

    /// The tab menu of tab `tab` (or of the bar), at a client point (`None`: under the tab).
    fn open_menu(&mut self, tab: Option<usize>, at: Option<(i32, i32)>) {
        let keys = |a| {
            self.matcher
                .keys_for(a)
                .map_or_else(String::new, |k| format!("\t{k}"))
        };
        let mut items: Vec<(u32, String, bool)> = Vec::new();
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
                .map(|(id, label, enabled)| json!({"id": id, "label": label, "enabled": enabled}))
                .collect::<Vec<_>>()}));
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

    fn copy(&mut self) {
        if !self.model.phase.shows_pane() {
            return;
        }
        let Some(text) = self.pane.selected_text() else {
            return;
        };
        if self.opts.test {
            self.test_clipboard = text;
        } else {
            set_clipboard(self.hwnd, &text.replace('\n', "\r\n"));
        }
        self.pane.clear_selection();
        self.redraw();
    }

    fn paste(&mut self) {
        if !matches!(self.model.phase, Phase::Attached { .. }) {
            return;
        }
        let text = if self.opts.test {
            Some(self.test_clipboard.clone())
        } else {
            clipboard(self.hwnd)
        };
        if let Some(t) = text.filter(|t| !t.is_empty()) {
            self.paste_text(&t);
        }
    }

    fn paste_text(&mut self, text: &str) {
        let bytes = self.pane.paste(text);
        self.input(&bytes);
        self.redraw();
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

    fn on_mouse(&mut self, msg: u32, wp: WPARAM, x: i32, y: i32) {
        let m = Self::mouse_mods(wp);
        let top = self.top() as i32;
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
                    }
                    self.redraw();
                }
            }
            WM_RBUTTONDOWN => {
                // Windows Terminal's right click: copy a selection, else paste.
                if self.pane.has_selection() {
                    self.copy();
                } else {
                    self.paste();
                }
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
        json!({
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
            "font_size": self.prefs.font_size,
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
            "clipboard": self.test_clipboard,
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
        })
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
                    // As TranslateMessage would: an unhandled key with a character types it.
                    if !handled && let Some(c) = cmd["char"].as_str().and_then(|c| c.chars().next())
                    {
                        self.type_char(c, m.alt, m);
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
                if let Some(t) = cmd["text"].as_str() {
                    self.test_clipboard = t.to_string();
                }
                self.paste();
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
                self.set_font_size(Prefs::zoomed(self.prefs.font_size, step.signum() as i32));
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
// Clipboard (never touched by the test hooks).

fn clipboard(hwnd: HWND) -> Option<String> {
    // SAFETY: the clipboard is opened by our window and closed on every path; the data is read
    // while its handle is locked.
    unsafe {
        if OpenClipboard(hwnd) == 0 {
            return None;
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
        text
    }
}

fn set_clipboard(hwnd: HWND, text: &str) -> bool {
    let units: Vec<u16> = text.encode_utf16().chain([0]).collect();
    // SAFETY: a movable global block of the right size, filled while locked; ownership passes to
    // the clipboard on success.
    unsafe {
        if OpenClipboard(hwnd) == 0 {
            return false;
        }
        EmptyClipboard();
        let h = GlobalAlloc(GMEM_MOVEABLE, units.len() * 2);
        let mut ok = false;
        if !h.is_null() {
            let p = GlobalLock(h).cast::<u16>();
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(units.as_ptr(), p, units.len());
                GlobalUnlock(h);
                ok = !SetClipboardData(u32::from(CF_UNICODETEXT), h as HANDLE).is_null();
            }
        }
        CloseClipboard();
        ok
    }
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
                After::Menu { items, at, tab } => {
                    let menu = CreatePopupMenu();
                    if menu.is_null() {
                        continue;
                    }
                    for (id, label, enabled) in &items {
                        if *id == 0 {
                            AppendMenuW(menu, MF_SEPARATOR, 0, null());
                        } else {
                            let w = wide(label);
                            let flags = if *enabled {
                                MF_STRING
                            } else {
                                MF_STRING | MF_GRAYED
                            };
                            AppendMenuW(menu, flags, *id as usize, w.as_ptr());
                        }
                    }
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
            }
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
            let hc = high_contrast_on();
            if hc != app.high_contrast || msg == WM_SYSCOLORCHANGE {
                app.high_contrast = hc;
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
            strip: Frame::new(1, 1, tokens::BG_RAISED),
            height: 0,
            pad_x: 0,
            first: 0,
        },
        rename: None,
        confirm: None,
        reported_tabs: Value::Null,
        normal: None,
        test_clipboard: String::new(),
        title: String::new(),
        frames: 0,
        bytes: 0,
        after: Vec::new(),
    };
    app.renderer = make_renderer(app.theme(), app.prefs.font_size, layout::scale(dpi), 8.0);
    app.renderer.set_focused(false);
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
    // SAFETY: Windows 10 20H1+ dark title bar; the caption, text and border colours apply on
    // Windows 11 and are ignored (an error result) on Windows 10. Our own window.
    unsafe {
        let dark: i32 = 1;
        DwmSetWindowAttribute(hwnd, DWMWA_DARK, (&raw const dark).cast(), 4);
        for (attr, c) in [
            (DWMWA_CAPTION, tokens::BG_BASE),
            (DWMWA_TEXT, tokens::TEXT_SECONDARY),
            (DWMWA_BORDER, tokens::BORDER_SUBTLE),
        ] {
            let v = colorref(c);
            DwmSetWindowAttribute(hwnd, attr, (&raw const v).cast(), 4);
        }
    }
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
