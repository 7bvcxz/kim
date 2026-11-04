//! The engine host that `ddalgak-gui.exe` runs (ADR 0022 §3, ADR 0024 §4).
//!
//! One user-started process with a hidden window (never shown) that owns:
//!
//! - the **lifecycle** (`off`/`running`/`paused`) and its side effects: installing and removing
//!   the hook, releasing held keys, the crash and emergency markers;
//! - the **tray icon** showing the state (`DDALGAK-SEC-010`);
//! - Ddalgak's **global shortcuts** (`engine.toggle`, `engine.pause`), registered with
//!   `RegisterHotKey` on that window so they work while the hook is off (`DDALGAK-FR-004`);
//! - **session notifications** (lock, unlock, disconnect, sign-out): held keys are released and no
//!   rule fires while the session is locked (DDALGAK_SAFETY §4);
//! - the **context watchers** (foreground integrity, password edits, IME windows) while the engine
//!   is on;
//! - the **IPC server** thread for `ddalgak status|health|engine …|quit`.
//!
//! The main thread sleeps in `GetMessageW`; the hook and worker threads sleep in their own waits.
//! Nothing polls (`DDALGAK-NFR-001`). The engine never turns itself on: only the user (tray, CLI,
//! `engine.toggle`) or the opt-in `engine_on_start: on` at a clean start.

use super::context::Watcher;
use super::hook::{self, HookThread, STATE_OFF, STATE_PAUSED, STATE_RUNNING};
use super::inject::SendInputBackend;
use super::pipe::{self, Listener};
use super::tray::{MenuCmd, Tray};
use super::{instance, process_cpu_us, wide};
use crate::inject::Injector;
use crate::ipc::{self, Method};
use crate::lifecycle::{
    Lifecycle, Markers, OffReason, Origin, Refusal, Request, StartFacts, State,
};
use crate::paths::Paths;
use crate::rules::{self, FileState};
use crate::shortcuts::Shortcuts;
use crate::stats::Stats;
use crate::table::{EngineOp, Table};
use crate::worker::{Activity, Control, Msg, Rules, Worker};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{GetLastError, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::RemoteDesktop::{
    NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification, WTSUnRegisterSessionNotification,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, MSG,
    PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW, TranslateMessage,
    WM_APP, WM_CLOSE, WM_CONTEXTMENU, WM_DESTROY, WM_ENDSESSION, WM_HOTKEY, WM_LBUTTONUP,
    WM_QUERYENDSESSION, WM_RBUTTONUP, WM_WTSSESSION_CHANGE, WNDCLASSW, WS_EX_TOOLWINDOW,
    WS_OVERLAPPED,
};

const WM_HOST_CMD: u32 = WM_APP + 1;
const WM_HOST_CONTROL: u32 = WM_APP + 2;
const WM_TRAY: u32 = WM_APP + 3;

// WTS session change codes (documented WM_WTSSESSION_CHANGE wParam values).
const WTS_CONSOLE_CONNECT: usize = 0x1;
const WTS_CONSOLE_DISCONNECT: usize = 0x2;
const WTS_REMOTE_CONNECT: usize = 0x3;
const WTS_REMOTE_DISCONNECT: usize = 0x4;
const WTS_SESSION_LOGOFF: usize = 0x6;
const WTS_SESSION_LOCK: usize = 0x7;
const WTS_SESSION_UNLOCK: usize = 0x8;

/// How the host runs.
#[derive(Clone, Debug)]
pub struct Options {
    pub paths: Paths,
    pub endpoint: String,
    /// Single-instance mutex name.
    pub instance: String,
    /// Show the tray icon (always, except the debug test host).
    pub tray: bool,
    /// Never consume keys (the debug test host).
    pub observe_only: bool,
    /// `--no-hotkeys` / `KIM_NO_HOTKEYS`.
    pub no_hotkeys: bool,
    /// Started by the opt-in sign-in entry (`--startup`).
    pub startup: bool,
    /// End after this long (the debug test host never outlives its test).
    pub max_lifetime: Option<Duration>,
}

/// Why the host did not run.
#[derive(Debug)]
pub enum RunError {
    /// Another instance owns this user session (`app_already_running`).
    AlreadyRunning,
    Io(String),
}

type Reply = Sender<Result<Value, (String, String)>>;

enum HostCmd {
    Ipc {
        method: Method,
        dry_run: bool,
        reply: Reply,
    },
}

struct Host {
    opts: Options,
    hwnd: HWND,
    lifecycle: Lifecycle,
    loaded: rules::Loaded,
    generation: u32,
    shared: Arc<hook::Shared>,
    stats: Arc<Stats>,
    worker: Worker,
    activity: Arc<Mutex<Activity>>,
    hook: Option<HookThread>,
    watcher: Option<Watcher>,
    injector: Injector<SendInputBackend>,
    markers: Markers,
    tray: Option<Tray>,
    shortcuts: Shortcuts,
    registered: Vec<(i32, String, &'static str)>,
    elevated: bool,
    started_at: String,
    started: Instant,
    start_reason: OffReason,
    locked: bool,
    cmd_rx: Receiver<HostCmd>,
    control_rx: Receiver<Control>,
    taskbar_created: u32,
}

thread_local! {
    static HOST: RefCell<Option<Host>> = const { RefCell::new(None) };
}

fn with_host<R>(f: impl FnOnce(&mut Host) -> R) -> Option<R> {
    HOST.with(|h| h.try_borrow_mut().ok()?.as_mut().map(f))
}

fn refusal(r: Refusal) -> (String, String) {
    (r.code().to_string(), r.to_string())
}

impl Host {
    fn state(&self) -> State {
        self.lifecycle.state()
    }

    fn notify(&self, title: &str, text: &str) {
        if let Some(t) = &self.tray {
            t.notify(title, text);
        }
    }

    /// Runs one engine request with all its side effects.
    fn engine(
        &mut self,
        req: Request,
        origin: &Origin,
        dry_run: bool,
    ) -> Result<Value, (String, String)> {
        if req == Request::Start && self.state() == State::Off {
            if self.elevated {
                return Err((
                    "engine_elevated".into(),
                    "Ddalgak runs as administrator; it does not turn its engine on elevated, \
                     because an elevated hook and injector would reach every window"
                        .into(),
                ));
            }
            // Each start reads rules.json again (the live reload is Step 67).
            self.loaded = rules::load(&self.opts.paths.rules());
            match &self.loaded.state {
                FileState::Invalid(why) => {
                    return Err((
                        "rules_invalid".into(),
                        format!("rules.json is not usable: {why}"),
                    ));
                }
                FileState::NewerVersion(v) => {
                    return Err((
                        "rules_from_newer_version".into(),
                        format!("rules.json has schema_version {v}, written by a newer Ddalgak"),
                    ));
                }
                _ => {}
            }
        }
        let plan = self.lifecycle.plan(req, origin).map_err(refusal)?;
        if dry_run {
            return Ok(json!({
                "dry_run": true,
                "would": { "request": req.as_str(), "from": plan.from.as_str(), "to": plan.to.as_str(), "changed": plan.changed() },
                "engine": self.engine_json(),
            }));
        }
        let change = self.lifecycle.apply(req, origin).map_err(refusal)?;
        if change.installs_hook() {
            self.generation = self.generation.wrapping_add(1);
            let bindings = self.loaded.bindings.clone();
            self.worker.queue().send(Msg::Rules(Rules {
                generation: self.generation,
                bindings: Arc::new(bindings.clone()),
                profile: self.loaded.profile.clone(),
                mode: self.loaded.mode.clone(),
            }));
            self.shared.set_state(STATE_RUNNING);
            let table = Table::compile(bindings, self.loaded.max_runs_per_second);
            match HookThread::start(
                table,
                self.generation,
                &self.shared,
                self.worker.queue(),
                hook::Options {
                    observe_only: self.opts.observe_only,
                    max_lifetime: None,
                },
            ) {
                Ok(h) => self.hook = Some(h),
                Err(e) => {
                    self.shared.set_state(STATE_OFF);
                    let _ = self.lifecycle.apply(Request::Stop, &Origin::Shutdown);
                    return Err((
                        "hook_failed".into(),
                        format!("Windows refused the keyboard hook: {e}"),
                    ));
                }
            }
            self.watcher = Some(Watcher::install(Arc::clone(&self.shared)));
            let _ = self.markers.engine_on();
        }
        self.shared.set_state(match change.to {
            State::Off => STATE_OFF,
            State::Running => STATE_RUNNING,
            State::Paused => STATE_PAUSED,
        });
        if change.releases_keys() {
            self.injector.release_all();
            self.worker.queue().send(Msg::Cancel);
            if let Some(h) = &self.hook {
                h.cancel_pending();
            }
        }
        if change.removes_hook() {
            if let Some(h) = self.hook.take() {
                h.stop();
            }
            self.watcher = None;
            let _ = if req == Request::Emergency {
                self.markers.emergency_off()
            } else {
                self.markers.engine_off()
            };
        }
        if let Some(t) = &mut self.tray {
            t.update(self.lifecycle.state());
        }
        if req == Request::Emergency {
            self.notify(
                "Ddalgak turned the engine off",
                "The emergency chord removed the keyboard hook and released every key. Turn the engine on again from this icon when you are ready.",
            );
        }
        Ok(json!({
            "dry_run": false,
            "change": { "request": req.as_str(), "from": change.from.as_str(), "to": change.to.as_str(), "changed": change.changed() },
            "engine": self.engine_json(),
        }))
    }

    fn engine_json(&self) -> Value {
        let last = self
            .lifecycle
            .last()
            .map(|(r, o)| json!({ "request": r.as_str(), "origin": o }));
        json!({
            "state": self.state().as_str(),
            "off_reason": self.lifecycle.off_reason().map(OffReason::as_str),
            "last_change": last,
            "hook": {
                "installed": self.shared.installed.load(Ordering::SeqCst),
                "observe_only": self.opts.observe_only,
            },
        })
    }

    fn registration(&self, action: &str) -> Option<&'static str> {
        self.registered
            .iter()
            .find(|(_, a, _)| a == action)
            .map(|(_, _, s)| *s)
    }

    fn status(&self) -> Value {
        let activity = self
            .activity
            .lock()
            .map(|a| a.to_json(20))
            .unwrap_or_else(|_| json!([]));
        json!({
            "app": {
                "running": true,
                "pid": std::process::id(),
                "version": crate::VERSION,
                "started_at": self.started_at,
                "uptime_s": self.started.elapsed().as_secs(),
                "elevated": self.elevated,
                "launched_by": if self.opts.startup { "startup" } else { "user" },
                "tray": self.tray.is_some(),
                "test_mode": !self.opts.tray,
            },
            "engine": self.engine_json(),
            "start": { "off_reason": self.start_reason.as_str() },
            "profile": { "id": self.loaded.profile, "mode": self.loaded.mode },
            "rules": rules::to_json(&self.loaded),
            "shortcuts": self.shortcuts.to_json(&|a| self.registration(a)),
            "session": { "locked": self.locked },
            "activity": activity,
        })
    }

    fn health(&self) -> Value {
        let mut problems = Vec::new();
        let state = self.state();
        let alive = self.hook.as_ref().is_some_and(HookThread::alive);
        if state.has_hook() && !alive {
            problems.push(json!({ "code": "hook_thread_ended", "message": "the engine is on but its hook thread ended; turn the engine off and on again" }));
        }
        let st = &self.stats;
        if st.late.load(Ordering::Relaxed) > 0 || st.max_us.load(Ordering::Relaxed) >= 300_000 {
            problems.push(json!({ "code": "hook_may_have_been_removed", "message": "a key event reached the hook very late; Windows may have removed the hook silently. Turn the engine off and on again (`ddalgak engine stop`, then `start`); Ddalgak never reinstalls it by itself" }));
        }
        if st.dropped.load(Ordering::Relaxed) > 0 {
            problems.push(json!({ "code": "signals_dropped", "message": "an event storm filled the worker queue; some rule runs were dropped" }));
        }
        if st.panics.load(Ordering::Relaxed) > 0 {
            problems.push(json!({ "code": "callback_panicked", "message": "the hook callback failed and passed the event on" }));
        }
        if !self.loaded.state.usable() {
            problems.push(json!({ "code": "rules_invalid", "message": "rules.json is not usable; the engine cannot be turned on with it" }));
        }
        for (_, a, s) in &self.registered {
            if *s == "taken" {
                problems.push(json!({ "code": "shortcut_taken", "message": format!("another program owns the chord of `{a}`") }));
            }
        }
        json!({
            "healthy": problems.is_empty(),
            "problems": problems,
            "engine": {
                "state": state.as_str(),
                "hook": {
                    "installed": self.shared.installed.load(Ordering::SeqCst),
                    "thread_alive": alive,
                    "observe_only": self.opts.observe_only,
                },
                "context_watchers": self.watcher.as_ref().map_or(0, Watcher::count),
            },
            "counters": st.to_json(),
            "process": {
                "pid": std::process::id(),
                "cpu_ms": process_cpu_us() / 1000,
                "uptime_s": self.started.elapsed().as_secs(),
            },
        })
    }

    fn ipc(&mut self, method: Method, dry_run: bool) -> Result<Value, (String, String)> {
        match method {
            Method::Status => Ok(self.status()),
            Method::Health => Ok(self.health()),
            Method::Engine(req) => self.engine(req, &Origin::Cli, dry_run),
            Method::Quit => {
                if !dry_run {
                    // SAFETY: posts a plain message to this thread's own window.
                    unsafe { PostMessageW(self.hwnd, WM_CLOSE, 0, 0) };
                }
                Ok(
                    json!({ "dry_run": dry_run, "quitting": !dry_run, "pid": std::process::id(),
                           "engine": self.engine_json() }),
                )
            }
        }
    }

    fn control(&mut self, c: Control) {
        let (req, origin) = match c {
            Control::Emergency => (Request::Emergency, Origin::Emergency),
            Control::Engine { op, rule } => (
                match op {
                    EngineOp::Pause => Request::Pause,
                    EngineOp::Resume => Request::Resume,
                    EngineOp::TogglePause => Request::TogglePause,
                    EngineOp::Disable => Request::Stop,
                },
                Origin::Rule(rule),
            ),
        };
        let _ = self.engine(req, &origin, false);
    }

    fn hotkey(&mut self, id: i32) {
        let Some(action) = self
            .registered
            .iter()
            .find(|(i, _, _)| *i == id)
            .map(|(_, a, _)| a.clone())
        else {
            return;
        };
        let origin = Origin::Shortcut(if action == "engine.toggle" {
            "engine.toggle"
        } else {
            "engine.pause"
        });
        let result = match (action.as_str(), self.state()) {
            ("engine.toggle", State::Off) => self.engine(Request::Start, &origin, false),
            ("engine.toggle", _) => self.engine(Request::Stop, &origin, false),
            (_, State::Off) => Err(refusal(Refusal::EngineOff)),
            _ => self.engine(Request::TogglePause, &origin, false),
        };
        if let Err((_, msg)) = result {
            self.notify("Ddalgak", &msg);
        }
    }

    fn menu(&mut self, cmd: MenuCmd) {
        let result = match (cmd, self.state()) {
            (MenuCmd::ToggleEngine, State::Off) => {
                self.engine(Request::Start, &Origin::Tray, false)
            }
            (MenuCmd::ToggleEngine, _) => self.engine(Request::Stop, &Origin::Tray, false),
            (MenuCmd::TogglePause, _) => self.engine(Request::TogglePause, &Origin::Tray, false),
            (MenuCmd::Quit, _) => {
                // SAFETY: posts a plain message to this thread's own window.
                unsafe { PostMessageW(self.hwnd, WM_CLOSE, 0, 0) };
                Ok(Value::Null)
            }
        };
        if let Err((_, msg)) = result {
            self.notify("Ddalgak", &msg);
        }
    }

    fn session(&mut self, code: usize) {
        match code {
            WTS_SESSION_LOCK
            | WTS_CONSOLE_DISCONNECT
            | WTS_REMOTE_DISCONNECT
            | WTS_SESSION_LOGOFF => {
                self.locked = true;
                self.shared.set_secure_desktop(true);
                self.injector.release_all();
                self.worker.queue().send(Msg::Cancel);
                if let Some(h) = &self.hook {
                    h.cancel_pending();
                }
            }
            WTS_SESSION_UNLOCK | WTS_CONSOLE_CONNECT | WTS_REMOTE_CONNECT => {
                self.locked = false;
                self.shared.set_secure_desktop(false);
                self.shared.request_key_reset();
            }
            _ => {}
        }
    }

    /// Engine off, shortcuts released: before the window goes away.
    fn shutdown(&mut self) {
        let _ = self.engine(Request::Stop, &Origin::Shutdown, false);
        for (id, _, s) in &self.registered {
            if *s == "registered" {
                // SAFETY: a hotkey this window registered.
                unsafe { UnregisterHotKey(self.hwnd, *id) };
            }
        }
        self.registered.clear();
        self.tray = None;
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_HOST_CMD => {
            let cmds: Vec<HostCmd> =
                with_host(|h| h.cmd_rx.try_iter().collect()).unwrap_or_default();
            for HostCmd::Ipc {
                method,
                dry_run,
                reply,
            } in cmds
            {
                let r = with_host(|h| h.ipc(method, dry_run))
                    .unwrap_or_else(|| Err(("app_unresponsive".into(), "the app is busy".into())));
                let _ = reply.send(r);
            }
            0
        }
        WM_HOST_CONTROL => {
            let controls: Vec<Control> =
                with_host(|h| h.control_rx.try_iter().collect()).unwrap_or_default();
            for c in controls {
                with_host(|h| h.control(c));
            }
            0
        }
        WM_TRAY => {
            let event = u32::try_from(lparam & 0xFFFF).unwrap_or(0);
            if matches!(event, WM_RBUTTONUP | WM_LBUTTONUP | WM_CONTEXTMENU) {
                // The menu runs a modal loop that dispatches messages: no borrow across it.
                if let Some(state) = with_host(|h| h.state())
                    && let Some(cmd) = Tray::menu(hwnd, state)
                {
                    with_host(|h| h.menu(cmd));
                }
            }
            0
        }
        WM_HOTKEY => {
            with_host(|h| h.hotkey(i32::try_from(wparam).unwrap_or(-1)));
            0
        }
        WM_WTSSESSION_CHANGE => {
            with_host(|h| h.session(wparam));
            0
        }
        WM_QUERYENDSESSION => 1,
        WM_ENDSESSION => {
            if wparam != 0 {
                with_host(|h| {
                    let _ = h.engine(Request::Stop, &Origin::Session, false);
                });
            }
            0
        }
        WM_CLOSE => {
            with_host(Host::shutdown);
            // SAFETY: destroys this thread's own window.
            unsafe { DestroyWindow(hwnd) };
            0
        }
        WM_DESTROY => {
            // SAFETY: ends this thread's message loop.
            unsafe { PostQuitMessage(0) };
            0
        }
        m if with_host(|h| h.taskbar_created == m && m != 0).unwrap_or(false) => {
            with_host(|h| {
                if let Some(t) = &mut h.tray {
                    t.readd();
                }
            });
            0
        }
        // SAFETY: default processing for every other message.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn create_window() -> Result<HWND, RunError> {
    let class = wide("mouv.ddalgak.host");
    // SAFETY: this executable's module handle.
    let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
    let wc = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        hInstance: instance,
        lpszClassName: class.as_ptr(),
        ..Default::default()
    };
    // SAFETY: `wc` points to a NUL-terminated class name alive for the call. A second
    // registration in the same process (tests) fails harmlessly with ERROR_CLASS_ALREADY_EXISTS.
    unsafe { RegisterClassW(&raw const wc) };
    // SAFETY: a top-level tool window that is never shown (no WS_VISIBLE, never ShowWindow): it
    // exists only to receive tray, hotkey and session messages; no taskbar entry, no focus.
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class.as_ptr(),
            wide("Ddalgak").as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        return Err(RunError::Io(format!(
            "cannot create the app window (error {})",
            // SAFETY: read right after the failed call.
            unsafe { GetLastError() }
        )));
    }
    Ok(hwnd)
}

fn register_shortcuts(hwnd: HWND, s: &Shortcuts) -> Vec<(i32, String, &'static str)> {
    let mut out = Vec::new();
    for (i, sc) in s.wanted().enumerate() {
        let id = i32::try_from(i + 1).unwrap_or(1);
        let Some((mods, vk)) = sc
            .chord
            .and_then(|c| kim_hotkeys::register::win32_chord(&c).ok())
        else {
            continue;
        };
        // SAFETY: registers a hotkey for this thread's window; unregistered at shutdown. A chord
        // another program owns fails and stays inactive (`taken`); nothing is retried or stolen.
        let ok = unsafe { RegisterHotKey(hwnd, id, mods | MOD_NOREPEAT, vk) } != 0;
        out.push((
            id,
            sc.action.clone(),
            if ok { "registered" } else { "taken" },
        ));
    }
    out
}

fn ipc_loop(mut listener: Listener, hwnd: isize, cmd_tx: &Sender<HostCmd>, me: &str) {
    while let Ok(Some(conn)) = listener.accept() {
        let deadline = Instant::now() + Duration::from_millis(ipc::TIMEOUT_MS);
        let Ok(line) = pipe::read_line(&conn, ipc::MAX_REQUEST, deadline) else {
            continue;
        };
        // Identity of the client, from the connection's token (after the first read, as Windows
        // requires). Another user gets no answer at all.
        if conn.client_user_sid().ok().as_deref() != Some(me) {
            continue;
        }
        let response = match ipc::parse_request(&line) {
            Err((code, msg)) => ipc::err(code, &msg),
            Ok((method, dry_run)) => {
                let (tx, rx) = channel();
                let sent = cmd_tx
                    .send(HostCmd::Ipc {
                        method,
                        dry_run,
                        reply: tx,
                    })
                    .is_ok();
                // SAFETY: posts a plain message to the app window (a stale handle fails harmlessly).
                let posted = sent && unsafe { PostMessageW(hwnd as HWND, WM_HOST_CMD, 0, 0) } != 0;
                match posted.then(|| rx.recv_timeout(Duration::from_millis(ipc::TIMEOUT_MS))) {
                    Some(Ok(Ok(v))) => ipc::ok(&v),
                    Some(Ok(Err((c, m)))) => ipc::err(&c, &m),
                    _ => ipc::err("app_unresponsive", "the app did not answer in time"),
                }
            }
        };
        if conn.write_all_bytes(response.as_bytes()).is_ok() {
            // Wait (briefly) until the client has read the answer and closed its end.
            let _ = pipe::read_line(&conn, 16, Instant::now() + Duration::from_secs(1));
        }
    }
}

/// Runs the app until it is quit (tray, `ddalgak quit`, sign-out). Returns when the window is
/// gone and everything is released.
pub fn run(opts: Options) -> Result<(), RunError> {
    let Some(_instance) =
        instance::acquire(&opts.instance).map_err(|e| RunError::Io(e.to_string()))?
    else {
        return Err(RunError::AlreadyRunning);
    };
    let listener = Listener::bind(&opts.endpoint).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            RunError::AlreadyRunning
        } else {
            RunError::Io(format!("cannot serve the local endpoint: {e}"))
        }
    })?;
    let me = super::user_sid().map_err(|e| RunError::Io(e.to_string()))?;
    let markers = Markers::new(&opts.paths.local);
    let (unclean_exit, emergency) = markers.take_start_facts();
    let loaded = rules::load(&opts.paths.rules());
    let elevated = super::is_elevated();
    let facts = StartFacts {
        engine_on_start: loaded.engine_on_start,
        unclean_exit,
        emergency,
        no_hotkeys: opts.no_hotkeys,
        elevated,
    };
    let policy = crate::lifecycle::start_policy(facts);
    let start_reason = policy.err().unwrap_or(OffReason::Default);
    let hwnd = create_window()?;
    let stats = Arc::new(Stats::default());
    let shared = hook::Shared::new(Arc::clone(&stats));
    let (control_tx, control_rx) = channel::<Control>();
    let hwnd_bits = hwnd as isize;
    let control_tx = Mutex::new(control_tx);
    let activity = Arc::new(Mutex::new(Activity::default()));
    let worker = Worker::start(
        Arc::clone(&stats),
        Box::new(move |c| {
            if let Ok(tx) = control_tx.lock() {
                let _ = tx.send(c);
            }
            // SAFETY: posts a plain message to the app window.
            unsafe { PostMessageW(hwnd_bits as HWND, WM_HOST_CONTROL, 0, 0) };
        }),
        Arc::clone(&activity),
    );
    let tray = if opts.tray {
        Some(Tray::new(hwnd, WM_TRAY, State::Off).map_err(|e| RunError::Io(e.to_string()))?)
    } else {
        None
    };
    let shortcuts = Shortcuts::load(&opts.paths, opts.no_hotkeys);
    let registered = register_shortcuts(hwnd, &shortcuts);
    // SAFETY: registers this thread's window for this session's lock/unlock notifications.
    let wts = unsafe { WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) } != 0;
    // SAFETY: NUL-terminated message name.
    let taskbar_created = unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) };
    let (cmd_tx, cmd_rx) = channel::<HostCmd>();
    let stopper = listener.stopper();
    let ipc_thread = std::thread::Builder::new()
        .name("ddalgak-ipc".into())
        .spawn(move || ipc_loop(listener, hwnd_bits, &cmd_tx, &me))
        .map_err(|e| RunError::Io(e.to_string()))?;
    if let Some(limit) = opts.max_lifetime {
        std::thread::spawn(move || {
            std::thread::sleep(limit);
            // SAFETY: posts a plain message; harmless when the window is already gone.
            unsafe { PostMessageW(hwnd_bits as HWND, WM_CLOSE, 0, 0) };
        });
    }
    HOST.with(|h| {
        *h.borrow_mut() = Some(Host {
            opts,
            hwnd,
            lifecycle: Lifecycle::new(start_reason),
            loaded,
            generation: 0,
            shared,
            stats: Arc::clone(&stats),
            worker,
            activity,
            hook: None,
            watcher: None,
            injector: Injector::new(SendInputBackend),
            markers,
            tray,
            shortcuts,
            registered,
            elevated,
            started_at: crate::time::now_rfc3339(),
            started: Instant::now(),
            start_reason,
            locked: false,
            cmd_rx,
            control_rx,
            taskbar_created,
        });
    });
    if policy.is_ok() {
        with_host(|h| {
            if let Err((_, msg)) = h.engine(Request::Start, &Origin::Startup, false) {
                h.notify("Ddalgak", &msg);
            }
        });
    } else if matches!(
        start_reason,
        OffReason::UncleanExit | OffReason::Emergency | OffReason::Elevated
    ) {
        with_host(|h| {
            let text = match start_reason {
                OffReason::UncleanExit => {
                    "Ddalgak did not stop cleanly last time, so the engine stays off. Turn it on from this icon."
                }
                OffReason::Emergency => {
                    "The last stop was the emergency chord, so the engine stays off. Turn it on from this icon."
                }
                _ => "Ddalgak runs as administrator and keeps its engine off.",
            };
            h.notify("Ddalgak engine is off", text);
        });
    }
    // SAFETY: MSG is plain data; all-zero is a valid out-parameter.
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        // SAFETY: `msg` is a valid out-pointer; null window = all of this thread's messages.
        let r = unsafe { GetMessageW(&raw mut msg, std::ptr::null_mut(), 0, 0) };
        if r == 0 || r == -1 {
            break;
        }
        stats.host_wakeups.fetch_add(1, Ordering::Relaxed);
        // SAFETY: standard dispatch of a message this thread received.
        unsafe {
            TranslateMessage(&raw const msg);
            DispatchMessageW(&raw const msg);
        }
    }
    if wts {
        // SAFETY: undoes the registration above; the window may already be destroyed.
        unsafe { WTSUnRegisterSessionNotification(hwnd) };
    }
    stopper.set();
    let _ = ipc_thread.join();
    // Drops the worker, the injector (releases held keys) and the tray icon, if still there.
    HOST.with(|h| {
        if let Some(mut host) = h.borrow_mut().take() {
            host.shutdown();
        }
    });
    Ok(())
}
