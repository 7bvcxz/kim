//! The resident instance on Windows (`noeul run`; ADR 0023 §1, NOEUL-FR-002, NOEUL-FR-003).
//!
//! Start-up, in this order: per-monitor DPI awareness; **bind the pipe first** (the pipe is the
//! single-instance guard: a second start finds it taken, hands its request to the running
//! instance over IPC and exits, so two instances never register the same hotkeys); a hidden
//! control window (never shown, no taskbar entry); the shortcuts (registered only after the
//! pipe is ours); the optional tray icon; the IPC acceptor; the `hotkeys.json` change watcher.
//!
//! Threads: the UI thread (message loop, every window, the [`App`] core), the hotkey thread of
//! `kim-hotkeys`, the IPC acceptor and one thread per connection, and the file watcher. Every
//! other thread only queues work and posts one message to the control window; the UI thread
//! does the work. Idle, every thread is blocked in the kernel (`GetMessageW`,
//! `ConnectNamedPipe`, `WaitForMultipleObjects`, a channel receive): no timer, no polling
//! (NOEUL-NFR-001).
//!
//! Tear-down on any exit path (IPC `quit`, tray Quit, `WM_CLOSE`, end of session): the core
//! cancels the capture, closes every pin and releases the global hotkeys; then the tray icon is
//! removed, the listener and watcher stop, and the process exits. A crash leaves nothing behind
//! either: Windows releases the hotkeys and removes the pipe with the process, so the next start
//! works (tested).
//!
//! **Debug-build test hook** (ADR 0026): with `NOEUL_DEV_SYNTHETIC_DESKTOP=1`, a *debug* build
//! captures from [`Synthetic::off_screen`] instead of the screen, and its overlay and pins never
//! take focus or stay on top. Everything it shows lies far off-screen, nothing reads the real
//! screen, and release builds do not contain the hook. Tests and the e2e suite use it to drive the
//! real binary's overlay without anything appearing on the user's desktop. Since Step 51 the
//! hook also keeps the outputs off the user's things: Copy goes to an in-memory stand-in (never
//! the clipboard; `NOEUL_DEV_CLIPBOARD=busy` makes it fail like a held clipboard), the
//! selection's context menu is recorded instead of shown, the save dialog is answered with a new
//! file in the configured quick-save folder (never shown), and quick save never falls back to
//! the user's Pictures folder.
//!
//! **Detection** (Step 52, ADR 0035): the window list is read on the UI thread when a capture
//! starts ([`crate::win::detect::snapshot`]); element lookups run on a UI Automation worker that
//! exists only while an overlay uses element detection, and its answers reach the core through
//! the pump. Under the test hook the list holds only windows of the processes named in
//! `NOEUL_DEV_DETECT_PIDS` (a test's own off-screen windows); without it, none — the hook never
//! looks at the user's windows.
//!
//! **Pins** (Step 53, ADR 0029): a pin's context menu is modal, so the pump only records the
//! request and [`wake`] shows it after releasing the core (one menu at a time), as for the tray
//! menu. `WM_DISPLAYCHANGE` and a work-area change (`WM_SETTINGCHANGE` with `SPI_SETWORKAREA`)
//! reach the core as [`App::display_changed`]: pins keep their place, nothing moves by itself.

use crate::app::{App, Identity, OverlayEvent, OverlayWindow, Shell};
use crate::capture::{CaptureBackend, Synthetic};
use crate::detect::{ElementRequest, Snapshot};
use crate::error::{Code, NoeulError, Result};
use crate::frame::{Frame, Ledger, Rect};
use crate::hotkeys::{self, Manager};
use crate::ipc;
use crate::paths::Paths;
use crate::pins::PinWindows;
use crate::settings;
use crate::win::detect::{ElementWorker, Scope};
use crate::win::overlay::{self, Placement, WinOverlay};
use crate::win::pinwnd::{self, Style, WinPins};
use crate::win::pipe::{self, Listener, Pipe, Until};
use crate::win::tray::TrayIcon;
use crate::win::{Event, client, gdi, per_monitor_dpi_awareness, session_id, user_sid, wide};
use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WAIT_OBJECT_0, WPARAM};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE,
    FindCloseChangeNotification, FindFirstChangeNotificationW, FindNextChangeNotification,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{INFINITE, WaitForMultipleObjects};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW,
    IDYES, MB_ICONQUESTION, MB_SETFOREGROUND, MB_YESNO, MSG, MessageBoxW, PostMessageW,
    PostQuitMessage, RegisterClassExW, RegisterWindowMessageW, SPI_SETWORKAREA, TranslateMessage,
    WM_APP, WM_CLOSE, WM_DISPLAYCHANGE, WM_ENDSESSION, WM_QUERYENDSESSION, WM_SETTINGCHANGE,
    WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_POPUP,
};

/// Runs the pump (IPC jobs, hotkey presses, reloads, pin events).
const WM_NOEUL_WAKE: u32 = WM_APP + 0x4E; // 'N'
/// Tray icon callbacks.
const WM_NOEUL_TRAY: u32 = WM_APP + 0x4F;
/// Most connections served at once; more are refused with `busy`.
const MAX_CONNECTIONS: usize = 16;
/// An idle connection is closed after this long without a request.
const IDLE: Duration = Duration::from_secs(60);

/// What `noeul run` was asked to do.
#[derive(Debug, Clone)]
pub struct RunOpts {
    pub endpoint: String,
    pub no_tray: bool,
    pub no_hotkeys: bool,
}

struct Job {
    method: String,
    params: Value,
    reply: Sender<Result<Value>>,
}

struct Inbox {
    jobs: Receiver<Job>,
    reload: Arc<AtomicBool>,
}

thread_local! {
    static APP: RefCell<Option<App<WinShell>>> = const { RefCell::new(None) };
    static INBOX: RefCell<Option<Inbox>> = const { RefCell::new(None) };
    static TRAY: RefCell<Option<TrayIcon>> = const { RefCell::new(None) };
    static CONTROL: Cell<usize> = const { Cell::new(0) };
    static TASKBAR_CREATED: Cell<u32> = const { Cell::new(0) };
    static ENDED_BY: Cell<&'static str> = const { Cell::new("quit") };
    static MENU_OPEN: Cell<bool> = const { Cell::new(false) };
}

/// Where Copy writes.
enum CopyTarget {
    /// The Windows clipboard, owned by the control window.
    System,
    /// The debug-build test hook: an in-memory stand-in, so automated runs never touch the
    /// user's clipboard; `busy` (`NOEUL_DEV_CLIPBOARD=busy`) makes every write fail as a
    /// clipboard held by another program does.
    Synthetic { busy: bool },
}

/// The Win32 [`Shell`]: pin windows, the overlay, clipboard, pointer, tray notices, detection.
struct WinShell {
    windows: WinPins,
    /// `false` only under the debug-build test hook.
    interactive: bool,
    copy: CopyTarget,
    /// Whose windows detection lists: all of them, or under the test hook only a test's own.
    detect_scope: Scope,
    /// The UI Automation worker of the open overlay, while it uses element detection.
    uia: Option<ElementWorker>,
}

impl WinShell {
    /// The element answers that arrived from the worker.
    fn element_answers(&self) -> Vec<(ElementRequest, crate::detect::ElementAnswer)> {
        self.uia
            .as_ref()
            .map(ElementWorker::take_answers)
            .unwrap_or_default()
    }
}

impl Shell for WinShell {
    fn pin_windows(&mut self) -> &mut dyn PinWindows {
        &mut self.windows
    }

    fn clipboard_image(&mut self, ledger: &Arc<Ledger>) -> Result<Frame> {
        crate::win::clipboard::read_image(ledger)
    }

    fn clipboard_write(&mut self, png: &[u8], dib: &[u8]) -> Result<()> {
        match self.copy {
            CopyTarget::System => {
                crate::win::clipboard::write_image(CONTROL.get() as HWND, png, dib)
            }
            CopyTarget::Synthetic { busy: true } => Err(NoeulError::new(
                Code::ClipboardUnavailable,
                "another program keeps the clipboard open (test hook)",
            )),
            CopyTarget::Synthetic { busy: false } => Ok(()),
        }
    }

    fn pictures_dir(&self) -> Option<std::path::PathBuf> {
        // Under the test hook nothing is ever written to the user's Pictures folder.
        if self.interactive {
            crate::win::files::pictures_dir()
        } else {
            None
        }
    }

    fn local_time(&self) -> [u16; 6] {
        crate::win::files::local_time()
    }

    fn pointer(&self) -> (i32, i32) {
        let mut p = POINT::default();
        // SAFETY: `p` is writable.
        unsafe { GetCursorPos(&raw mut p) };
        (p.x, p.y)
    }

    fn notice(&mut self, text: &str) {
        TRAY.with(|t| {
            if let Ok(t) = t.try_borrow()
                && let Some(t) = t.as_ref()
            {
                t.balloon(text);
            }
        });
    }

    fn tray_shown(&self) -> bool {
        TRAY.with(|t| {
            t.try_borrow()
                .map(|t| t.as_ref().is_some_and(TrayIcon::shown))
                .unwrap_or(false)
        })
    }

    fn confirm_quit(&mut self, pins: usize) -> bool {
        let text = wide(format!(
            "Quit Noeul? {pins} pin(s) will close. Pins are not backed up."
        ));
        let title = wide("Noeul");
        // SAFETY: a modal message box owned by our control window; strings are NUL-terminated.
        let r = unsafe {
            MessageBoxW(
                CONTROL.get() as HWND,
                text.as_ptr(),
                title.as_ptr(),
                MB_YESNO | MB_ICONQUESTION | MB_SETFOREGROUND,
            )
        };
        r == IDYES
    }

    fn open_overlay(
        &mut self,
        picture: Rect,
    ) -> std::result::Result<Box<dyn OverlayWindow>, String> {
        let placement = if self.interactive {
            Placement::INTERACTIVE
        } else {
            Placement::OFF_SCREEN_TEST
        };
        WinOverlay::create(picture, placement).map(|w| Box::new(w) as Box<dyn OverlayWindow>)
    }

    fn system_values(&self) -> crate::overlay::SystemValues {
        overlay::system_values()
    }

    fn interactive(&self) -> bool {
        self.interactive
    }

    fn window_snapshot(&mut self, picture: Rect) -> Snapshot {
        crate::win::detect::snapshot(picture, &self.detect_scope, overlay::current())
    }

    fn request_element(&mut self, req: ElementRequest) {
        if self.uia.is_none() {
            let control = CONTROL.get();
            self.uia = ElementWorker::start(Box::new(move || post_wake(control))).ok();
        }
        if let Some(w) = &self.uia {
            w.ask(req);
        }
    }

    fn end_detection(&mut self) {
        self.uia = None;
    }
}

/// Whose windows the debug-build test hook lists: the processes in `NOEUL_DEV_DETECT_PIDS`
/// (comma-separated), never anyone else's.
fn dev_detect_scope() -> Scope {
    let pids = std::env::var("NOEUL_DEV_DETECT_PIDS")
        .unwrap_or_default()
        .split(',')
        .filter_map(|p| p.trim().parse::<u32>().ok())
        .filter(|p| *p != 0)
        .collect();
    Scope::Processes(pids)
}

/// The debug-build test hook: `NOEUL_DEV_SYNTHETIC_DESKTOP=1` (ignored by release builds, and
/// when the off-screen layout would touch a real monitor).
fn dev_synthetic_desktop() -> bool {
    #[cfg(debug_assertions)]
    {
        let on = std::env::var_os("NOEUL_DEV_SYNTHETIC_DESKTOP").is_some_and(|v| v == "1");
        let real = gdi::desktop().virtual_rect;
        let fake = Synthetic::off_screen().desktop().virtual_rect;
        on && !matches!((real, fake), (Some(r), Some(f)) if r.intersects(&f))
    }
    #[cfg(not(debug_assertions))]
    {
        false
    }
}

fn post_wake(control: usize) {
    // SAFETY: posts a pointer-free message to our control window (fails harmlessly if gone).
    unsafe { PostMessageW(control as HWND, WM_NOEUL_WAKE, 0, 0) };
}

/// Does all queued work. Runs with the core borrowed; anything queued meanwhile (by nested
/// window messages) is picked up by the loop before it returns.
fn pump(app: &mut App<WinShell>) {
    loop {
        let mut did = false;
        let jobs: Vec<Job> = INBOX.with(|i| {
            i.borrow()
                .as_ref()
                .map(|i| i.jobs.try_iter().collect())
                .unwrap_or_default()
        });
        for job in jobs {
            did = true;
            let answer = catch_unwind(AssertUnwindSafe(|| app.request(&job.method, &job.params)))
                .unwrap_or_else(|_| {
                    Err(NoeulError::new(
                        Code::Internal,
                        "the request failed inside Noeul (a bug); Noeul keeps running",
                    ))
                });
            let _ = job.reply.send(answer);
        }
        while let Some(id) = app.hotkeys().take_press() {
            did = true;
            app.hotkey_pressed(id);
        }
        if INBOX.with(|i| {
            i.borrow()
                .as_ref()
                .is_some_and(|i| i.reload.swap(false, Ordering::SeqCst))
        }) {
            // Something changed in the settings folder: hotkeys.json and config.toml.
            did = true;
            app.hotkeys().reload();
            app.reload_settings();
        }
        for e in pinwnd::take_events() {
            did = true;
            app.pin_event(e);
        }
        for e in overlay::take_events() {
            did = true;
            app.overlay_event(e);
        }
        for (req, answer) in app.shell().element_answers() {
            did = true;
            app.overlay_event(OverlayEvent::Element(req, answer));
        }
        if !did {
            break;
        }
    }
    if app.quitting() {
        // SAFETY: ends this thread's message loop.
        unsafe { PostQuitMessage(0) };
    }
}

/// Runs `f` on the core and then the pump; does nothing while the core is already borrowed
/// (the outer borrower's pump handles what was queued).
fn with_app<R>(f: impl FnOnce(&mut App<WinShell>) -> R) -> Option<R> {
    APP.with(|a| {
        let mut g = a.try_borrow_mut().ok()?;
        let app = g.as_mut()?;
        let r = f(app);
        pump(app);
        Some(r)
    })
}

fn wake() {
    with_app(|_| ());
    pin_menus();
    overlay_requests();
}

/// Shows what the capture overlay asked for — the selection's context menu, the save dialog —
/// one at a time, with the core released (both are modal), then reports the answer. Under the
/// debug-build test hook the core never asks for a menu, and a save is answered without a
/// dialog: a new file in the explicitly configured quick-save folder, or "cancelled".
fn overlay_requests() {
    if MENU_OPEN.get() {
        return;
    }
    loop {
        if let Some(Some(menu)) = with_app(App::take_overlay_menu) {
            MENU_OPEN.set(true);
            let choice = overlay::current()
                .and_then(|h| pinwnd::track_menu_on(h, &menu.items, Some(menu.at)));
            MENU_OPEN.set(false);
            if let Some(action) = choice {
                with_app(|a| a.overlay_menu_choice(action));
            }
            continue;
        }
        if let Some(Some((req, interactive))) =
            with_app(|a| a.take_save_request().map(|r| (r, a.shell().interactive)))
        {
            let path = if interactive {
                // Owned by the pin being saved, or by the overlay, so it stays above it.
                let owner = match req.pin {
                    Some(id) => pinwnd::hwnd_of(id),
                    None => overlay::current(),
                };
                MENU_OPEN.set(true);
                let p = crate::win::files::save_dialog(owner, &req);
                MENU_OPEN.set(false);
                p
            } else if req.explicit {
                req.folder
                    .as_ref()
                    .map(|f| crate::output::free_name(f, &req.name))
            } else {
                None
            };
            with_app(|a| a.save_chosen(&req, path));
            continue;
        }
        break;
    }
}

/// Shows the pin context menus the core asked for, one at a time, with the core released (the
/// menu is modal; IPC and other pins keep working meanwhile), then runs the choice.
fn pin_menus() {
    if MENU_OPEN.get() {
        return; // the open menu's loop picks the next request up when it returns
    }
    while let Some(Some(menu)) = with_app(App::take_pin_menu) {
        MENU_OPEN.set(true);
        let choice = pinwnd::track_menu(menu.pin, &menu.items, menu.at);
        MENU_OPEN.set(false);
        if let Some(action) = choice {
            with_app(|a| a.pin_menu_choice(menu.pin, action));
        }
    }
}

unsafe extern "system" fn control_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NOEUL_WAKE => {
            wake();
            return 0;
        }
        WM_NOEUL_TRAY => {
            if let Some(click) = TrayIcon::click(lp) {
                match click {
                    crate::tray::Click::Middle | crate::tray::Click::Left => {
                        with_app(|a| a.tray_click(click));
                    }
                    crate::tray::Click::Right => {
                        // The menu is modal: build it, release the core, then run the choice.
                        if let Some(items) = with_app(|a| a.tray_menu()) {
                            let choice = TRAY.with(|t| {
                                t.try_borrow()
                                    .ok()
                                    .and_then(|t| t.as_ref().and_then(|t| t.menu(&items)))
                            });
                            if let Some(item) = choice {
                                with_app(|a| a.tray_command(item));
                            }
                        }
                    }
                }
            }
            return 0;
        }
        // Monitors attached, removed or rearranged; a taskbar moved or resized.
        WM_DISPLAYCHANGE => {
            with_app(App::display_changed);
            return 0;
        }
        WM_SETTINGCHANGE if wp == SPI_SETWORKAREA as usize => {
            with_app(App::display_changed);
            return 0;
        }
        WM_QUERYENDSESSION => return 1,
        WM_ENDSESSION => {
            if wp != 0 {
                ENDED_BY.set("end_of_session");
                with_app(App::shutdown);
                TRAY.with(|t| {
                    if let Ok(mut t) = t.try_borrow_mut() {
                        t.take();
                    }
                });
            }
            return 0;
        }
        WM_CLOSE => {
            // `taskkill` without /F, or another program asking politely: quit cleanly.
            ENDED_BY.set("closed");
            with_app(App::shutdown);
            return 0;
        }
        _ => {}
    }
    if msg != 0 && msg == TASKBAR_CREATED.get() {
        TRAY.with(|t| {
            if let Ok(mut t) = t.try_borrow_mut()
                && let Some(t) = t.as_mut()
            {
                t.readd();
            }
        });
        return 0;
    }
    // SAFETY: default processing of our own window's message.
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

fn control_window() -> Result<HWND> {
    static CLASS: OnceLock<std::result::Result<usize, String>> = OnceLock::new();
    let class = CLASS
        .get_or_init(|| {
            let name = wide("NoeulControl");
            let wc = WNDCLASSEXW {
                cbSize: u32::try_from(size_of::<WNDCLASSEXW>()).expect("small"),
                lpfnWndProc: Some(control_proc),
                // SAFETY: this executable's module handle.
                hInstance: unsafe { GetModuleHandleW(std::ptr::null()) },
                lpszClassName: name.as_ptr(),
                ..Default::default()
            };
            // SAFETY: `wc` is fully initialized.
            if unsafe { RegisterClassExW(&raw const wc) } == 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            Ok(Box::leak(name.into_boxed_slice()).as_ptr() as usize)
        })
        .clone()
        .map_err(|e| NoeulError::new(Code::Internal, format!("RegisterClassExW: {e}")))?;
    let title = wide("Noeul");
    // SAFETY: a hidden popup tool window (never shown): no taskbar entry, no Alt+Tab.
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class as *const u16,
            title.as_ptr(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            GetModuleHandleW(std::ptr::null()),
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        return Err(NoeulError::new(
            Code::Internal,
            format!(
                "cannot create the control window: {}",
                std::io::Error::last_os_error()
            ),
        ));
    }
    Ok(hwnd)
}

/// One IPC connection: handshake with the identity check, then requests in order.
fn serve_conn(p: &Pipe, sid: &str, jobs: &Sender<Job>, control: usize, welcome: &Value) {
    let close_with = |e: NoeulError| {
        let _ = p.write_all_bytes(&ipc::encode(&ipc::connection_error(&e)));
    };
    let mut r = Until {
        pipe: p,
        deadline: Some(Instant::now() + ipc::HANDSHAKE_TIMEOUT),
    };
    let first = match ipc::read_frame(&mut r) {
        Ok(Some(m)) => m,
        Ok(None) => return,
        Err(ipc::FrameError::Io(e)) if e.kind() == std::io::ErrorKind::TimedOut => {
            return close_with(NoeulError::new(
                Code::Timeout,
                format!("no hello within {} s", ipc::HANDSHAKE_TIMEOUT.as_secs()),
            ));
        }
        Err(e) => return close_with(NoeulError::new(Code::ProtocolError, e.to_string())),
    };
    // Who is on the other end: the token of this very connection (NOEUL-SEC-011).
    if !p.client_user_sid().is_ok_and(|s| s == sid) {
        return close_with(NoeulError::new(
            Code::PermissionDenied,
            "the client does not run as Noeul's user",
        ));
    }
    let hello = match ipc::parse_hello(&first) {
        Ok(h) => h,
        Err(e) => return close_with(e),
    };
    if ipc::negotiate(hello.min, hello.max).is_none() {
        return close_with(NoeulError::new(
            Code::NoeulIncompatible,
            format!(
                "this Noeul speaks IPC v{}..v{}, the client v{}..v{}",
                ipc::PROTOCOL_MIN,
                ipc::PROTOCOL_MAX,
                hello.min,
                hello.max
            ),
        ));
    }
    if p.write_all_bytes(&ipc::encode(welcome)).is_err() {
        return;
    }
    loop {
        let mut r = Until {
            pipe: p,
            deadline: Some(Instant::now() + IDLE),
        };
        let frame = match ipc::read_frame(&mut r) {
            Ok(Some(m)) => m,
            Ok(None) => return,
            Err(ipc::FrameError::Io(_)) => return,
            Err(e) => return close_with(NoeulError::new(Code::ProtocolError, e.to_string())),
        };
        let req = match ipc::parse_request(&frame) {
            Ok(r) => r,
            Err((0, e)) => return close_with(e),
            Err((id, e)) => {
                if p.write_all_bytes(&ipc::encode(&ipc::err(id, &e))).is_err() {
                    return;
                }
                continue;
            }
        };
        let (tx, rx) = channel();
        let sent = jobs
            .send(Job {
                method: req.method.clone(),
                params: req.params.clone(),
                reply: tx,
            })
            .is_ok();
        if sent {
            post_wake(control);
        }
        let answer = if sent {
            rx.recv_timeout(ipc::REQUEST_TIMEOUT - Duration::from_secs(1))
                .unwrap_or_else(|_| {
                    Err(NoeulError::new(
                        Code::NoeulUnresponsive,
                        "Noeul is busy (a dialog may be open) or shutting down",
                    ))
                })
        } else {
            Err(NoeulError::new(
                Code::NoeulUnresponsive,
                "Noeul is shutting down",
            ))
        };
        let quitting = req.method == "quit" && answer.as_ref().is_ok_and(|v| v["quitting"] == true);
        let msg = match &answer {
            Ok(v) => ipc::ok(req.id, v.clone()),
            Err(e) => ipc::err(req.id, e),
        };
        if p.write_all_bytes(&ipc::encode(&msg)).is_err() || quitting {
            return;
        }
    }
}

/// Watches the settings folder and asks for a reload when something changes (§8.3 5).
fn watch(dir: &std::path::Path, stop: &Event, reload: &AtomicBool, control: usize) {
    let w = wide(dir);
    // SAFETY: NUL-terminated folder name; the handle is closed below.
    let h = unsafe {
        FindFirstChangeNotificationW(
            w.as_ptr(),
            0,
            FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_SIZE,
        )
    };
    if h.is_null() || h == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
        return; // no watcher: IPC `hotkeys.reload` still works
    }
    loop {
        let handles = [h, stop.raw()];
        // SAFETY: two valid handles; blocks without polling.
        let w = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) };
        if w != WAIT_OBJECT_0 {
            break;
        }
        reload.store(true, Ordering::SeqCst);
        post_wake(control);
        // SAFETY: re-arms the notification handle created above.
        if unsafe { FindNextChangeNotification(h) } == 0 {
            break;
        }
    }
    // SAFETY: closes the notification handle created above.
    unsafe { FindCloseChangeNotification(h) };
}

/// `noeul run`: the instance, in this process, until it quits.
pub fn run(opts: &RunOpts) -> Result<Value> {
    per_monitor_dpi_awareness();
    let sid = user_sid().map_err(|e| {
        NoeulError::new(Code::Io, format!("cannot determine the current user: {e}"))
    })?;
    let mut listener = match Listener::bind(&opts.endpoint) {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // NOEUL-FR-003: hand the start over to the running instance and exit.
            let mut s = client::open(&opts.endpoint)?;
            let r = s.request("activate", json!({"reason": "start"}))?;
            return Ok(json!({"handed_over": true, "pid": s.pid, "activate": r}));
        }
        Err(e) => {
            return Err(NoeulError::new(
                Code::Io,
                format!("cannot create the Noeul pipe: {e}"),
            ));
        }
    };
    let paths = Paths::from_env()?;
    let control = control_window()?;
    let control_id = control as usize;
    CONTROL.set(control_id);
    // SAFETY: registers (or looks up) the documented broadcast name.
    TASKBAR_CREATED.set(unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) });

    let off = hotkeys::off_reason(opts.no_hotkeys);
    let mgr = Manager::start(&paths, off, || {
        kim_hotkeys::register::Registrar::start_with_waker(Box::new(move || post_wake(control_id)))
    });
    let settings = settings::load(&paths.config_file());
    if settings.settings.tray_icon
        && !opts.no_tray
        && let Ok(t) = TrayIcon::add(control, WM_NOEUL_TRAY)
    {
        TRAY.with(|s| *s.borrow_mut() = Some(t));
    }
    let endpoint_path = pipe::path(&opts.endpoint);
    let dev = dev_synthetic_desktop();
    let backend: Box<dyn CaptureBackend> = if dev {
        Box::new(Synthetic::off_screen())
    } else {
        Box::new(gdi::Gdi)
    };
    let app = App::new(
        WinShell {
            windows: WinPins {
                style: if dev {
                    Style::OFF_SCREEN_TEST
                } else {
                    Style::INTERACTIVE
                },
            },
            interactive: !dev,
            copy: if dev {
                CopyTarget::Synthetic {
                    busy: std::env::var_os("NOEUL_DEV_CLIPBOARD").is_some_and(|v| v == "busy"),
                }
            } else {
                CopyTarget::System
            },
            detect_scope: if dev { dev_detect_scope() } else { Scope::All },
            uia: None,
        },
        backend,
        mgr,
        settings,
        paths.clone(),
        Identity {
            pid: std::process::id(),
            endpoint: endpoint_path,
            session: session_id(),
        },
    );
    let welcome = ipc::welcome(ipc::PROTOCOL_MAX, std::process::id(), &app.started_at());
    let (jobs_tx, jobs_rx) = channel::<Job>();
    let reload = Arc::new(AtomicBool::new(false));
    INBOX.with(|i| {
        *i.borrow_mut() = Some(Inbox {
            jobs: jobs_rx,
            reload: Arc::clone(&reload),
        });
    });
    APP.with(|a| *a.borrow_mut() = Some(app));
    pinwnd::set_waker(Some(Box::new(wake)));
    overlay::set_waker(Some(Box::new(wake)));

    // IPC acceptor.
    let stopper = listener.stopper();
    let active = Arc::new(AtomicUsize::new(0));
    let acceptor = thread::Builder::new()
        .name("noeul-ipc".into())
        .spawn({
            let active = Arc::clone(&active);
            move || {
                let mut backoff = Duration::from_millis(50);
                loop {
                    match listener.accept() {
                        Ok(Some(p)) => {
                            backoff = Duration::from_millis(50);
                            if active.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
                                let e = NoeulError::new(
                                    Code::Busy,
                                    format!("Noeul serves at most {MAX_CONNECTIONS} connections"),
                                );
                                let _ = p.write_all_bytes(&ipc::encode(&ipc::connection_error(&e)));
                                continue;
                            }
                            active.fetch_add(1, Ordering::SeqCst);
                            let (sid, jobs, welcome, active) = (
                                sid.clone(),
                                jobs_tx.clone(),
                                welcome.clone(),
                                Arc::clone(&active),
                            );
                            let _ =
                                thread::Builder::new()
                                    .name("noeul-conn".into())
                                    .spawn(move || {
                                        serve_conn(&p, &sid, &jobs, control_id, &welcome);
                                        active.fetch_sub(1, Ordering::SeqCst);
                                    });
                        }
                        Ok(None) => return,
                        // Out of resources for a new instance: retry with a bounded backoff.
                        Err(_) => {
                            thread::sleep(backoff);
                            backoff = (backoff * 2).min(Duration::from_secs(1));
                        }
                    }
                }
            }
        })
        .map_err(|e| {
            NoeulError::new(Code::Internal, format!("cannot start the IPC thread: {e}"))
        })?;

    // hotkeys.json watcher (the folder must exist to be watched).
    let _ = std::fs::create_dir_all(&paths.config_dir);
    let watch_stop = Arc::new(Event::new().map_err(|e| NoeulError::io("event", &e))?);
    let watcher = thread::Builder::new()
        .name("noeul-watch".into())
        .spawn({
            let dir = paths.config_dir;
            let stop = Arc::clone(&watch_stop);
            let reload = Arc::clone(&reload);
            move || watch(&dir, &stop, &reload, control_id)
        })
        .ok();

    // The message loop.
    // SAFETY: MSG is plain data; all-zero is a valid out-parameter.
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        // SAFETY: `msg` is writable; null window = every message of this thread.
        let r = unsafe { GetMessageW(&raw mut msg, std::ptr::null_mut(), 0, 0) };
        if r == 0 || r == -1 {
            break;
        }
        // SAFETY: a message just retrieved by GetMessageW.
        unsafe {
            TranslateMessage(&raw const msg);
            DispatchMessageW(&raw const msg);
        }
    }

    // Tear-down (also after WM_ENDSESSION, which may not return here before the process ends).
    let (pins, held) = APP.with(|a| {
        a.borrow_mut().take().map_or((0, 0), |mut app| {
            let pins = app.pins().len();
            app.shutdown();
            let held = app.ledger().live().1;
            (pins, held)
        })
    });
    pinwnd::set_waker(None);
    overlay::set_waker(None);
    TRAY.with(|t| t.borrow_mut().take());
    INBOX.with(|i| i.borrow_mut().take());
    stopper.set();
    watch_stop.set();
    let until = Instant::now() + Duration::from_secs(2);
    while !acceptor.is_finished() && Instant::now() < until {
        thread::sleep(Duration::from_millis(10));
    }
    if let Some(w) = watcher {
        let _ = w.join();
    }
    // SAFETY: our own control window.
    unsafe { DestroyWindow(control) };
    Ok(json!({
        "handed_over": false,
        "pid": std::process::id(),
        "reason": ENDED_BY.get(),
        "open_pins_at_exit": pins,
        "image_bytes_at_exit": held,
    }))
}
