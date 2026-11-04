//! The low-level keyboard hook (`WH_KEYBOARD_LL`) on its own thread (ADR 0024 §3).
//!
//! - **One dedicated thread** installs the hook and runs a message loop; Windows calls the hook
//!   procedure on that thread. The thread exists only while the engine is `running` or
//!   `paused` (`DDALGAK-FR-002`), and only inside `ddalgak-gui.exe` or a test (`DDALGAK-SEC-010`).
//! - **The callback is minimal**: it copies `KBDLLHOOKSTRUCT` into a [`RawKey`], runs the
//!   allocation-free [`Core::decide`], pushes at most one small signal into the worker's bounded
//!   queue (`try_send`, never blocking), updates atomic counters and returns. No I/O, no locks
//!   that another thread holds for long, no allocation (`DDALGAK-NFR-002`). A panic inside is
//!   caught and the event passes on.
//! - **Every event not consumed goes to `CallNextHookEx`**. In `observe_only` mode (tests and the
//!   debug test host) nothing is ever consumed.
//! - **Cleanup on every path**: [`HookThread::stop`] and `Drop` post `WM_QUIT` and join; the
//!   thread unhooks before it ends, including when it unwinds; an emergency unhooks right after
//!   the callback that saw the chord; `max_lifetime` (tests) ends the hook after a deadline; and
//!   Windows removes the hook when the process ends.
//! - Table swaps and the engine state reach the thread without stopping it: the state is an
//!   atomic the callback reads, a new table is handed over by a posted message and swapped in
//!   between callbacks.
//! - **The low-level mouse hook** (`WH_MOUSE_LL`, Step 68, ADR 0031) is installed on the same
//!   thread only while the table has a mouse-button trigger (and removed when a reload drops the
//!   last one), so the user's `mouse-hook` permission is used only when a rule needs it. Its
//!   callback passes every move, wheel and primary/secondary button event on at once without
//!   looking at it; middle and X button events go through the same core as keys.
//! - **The dual-role timer**: when the core asks for it, a one-shot thread timer (`SetTimer`
//!   without a window) wakes the thread after the tap time; it exists only while a dual-role key
//!   is held undecided, so an idle engine still has no timer (`DDALGAK-NFR-001`).

use crate::core::{Core, Decision, Flags, Mode, Signal};
use crate::event::{RawKey, Source};
use crate::inject::Button;
use crate::modes::{ModeAction, ModeOrigin, ModeStatus};
use crate::stats::Stats;
use crate::table::Table;
use crate::worker::Queue;
use std::cell::RefCell;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows_sys::Win32::System::SystemInformation::GetTickCount;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, KillTimer, LLKHF_EXTENDED,
    LLKHF_INJECTED, LLKHF_LOWER_IL_INJECTED, LLKHF_UP, LLMHF_INJECTED, LLMHF_LOWER_IL_INJECTED,
    MSG, MSLLHOOKSTRUCT, PM_NOREMOVE, PeekMessageW, PostMessageW, PostThreadMessageW, SetTimer,
    SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_APP, WM_MBUTTONDOWN,
    WM_MBUTTONUP, WM_QUIT, WM_TIMER, WM_XBUTTONDOWN, WM_XBUTTONUP, XBUTTON1, XBUTTON2,
};

/// Engine state as the callback reads it.
pub const STATE_OFF: u8 = 0;
pub const STATE_RUNNING: u8 = 1;
pub const STATE_PAUSED: u8 = 2;

const WM_SWAP: u32 = WM_APP + 0x10;
const WM_UNHOOK: u32 = WM_APP + 0x11;
const WM_CANCEL: u32 = WM_APP + 0x12;
const WM_RESET: u32 = WM_APP + 0x13;
const WM_MODE: u32 = WM_APP + 0x14;

/// A mode request for the hook thread, which owns the mode state (Step 72).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModeRequest {
    /// Only read the state.
    Query,
    /// Switch to this mode (the CLI's `mode set`, the tray's mode menu).
    Set { mode: String, origin: ModeOrigin },
    /// Toggle this mode (`mode toggle`).
    Toggle { mode: String, origin: ModeOrigin },
    /// The safe reset to the default mode (`mode reset`, the tray's "back to" item).
    Reset { origin: ModeOrigin },
    /// Run the mode step of binding `binding` of the table `generation` (a rule whose `on: hold`
    /// trigger the worker timed); ignored for an older table.
    Rule { binding: u16, generation: u32 },
}

/// The hook thread's answer: the state afterwards and whether the active mode changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModeReply {
    pub from: String,
    pub status: ModeStatus,
    pub changed: bool,
}

type ModeResult = Result<ModeReply, (&'static str, String)>;

const F_SECURE: u32 = 1;
const F_PASSWORD: u32 = 2;
const F_IME: u32 = 4;
const F_ELEVATED: u32 = 8;

/// State shared between the host, the context watchers and the hook callback.
#[derive(Debug, Default)]
pub struct Shared {
    state: AtomicU8,
    flags: AtomicU32,
    focus_epoch: AtomicU32,
    /// The hook is installed right now.
    pub installed: AtomicBool,
    /// The low-level mouse hook is installed right now (only while a mouse-button rule exists).
    pub mouse_installed: AtomicBool,
    /// The hook thread, for key-state resets requested by the watchers.
    thread_id: AtomicU32,
    pub stats: Arc<Stats>,
    pending: Mutex<Option<(u32, Table)>>,
    /// A mode request and where to answer it (Step 72).
    mode_req: Mutex<Option<(ModeRequest, Sender<ModeResult>)>>,
    /// Step 73: the window told when the hook may have been removed ([`WM_HOOK_LOST`]).
    host_window: AtomicIsize,
    /// Step 73: a callback this long, or a physical event that waited this long, means Windows
    /// may have removed the hook (`LowLevelHooksTimeout`); 0 turns the check off.
    timeout_us: AtomicU64,
    /// Step 73: the loss was reported for the current hook (once per hook).
    lost: AtomicBool,
}

/// Step 73: posted to the host window when a callback reached the hook timeout
/// ([`crate::recovery::hook_lost`]).
pub const WM_HOOK_LOST: u32 = WM_APP + 0x20;

impl Shared {
    pub fn new(stats: Arc<Stats>) -> Arc<Shared> {
        Arc::new(Shared {
            stats,
            ..Shared::default()
        })
    }

    pub fn set_state(&self, s: u8) {
        self.state.store(s, Ordering::SeqCst);
    }

    /// Step 73: where a lost hook is reported ([`WM_HOOK_LOST`]).
    pub fn set_host_window(&self, hwnd: isize) {
        self.host_window.store(hwnd, Ordering::SeqCst);
    }

    /// Step 73: the hook-timeout threshold in µs (0: no check).
    pub fn set_hook_timeout_us(&self, us: u64) {
        self.timeout_us.store(us, Ordering::SeqCst);
    }

    pub fn hook_timeout_us(&self) -> u64 {
        self.timeout_us.load(Ordering::SeqCst)
    }

    /// Step 73: whether the current hook was reported lost.
    pub fn lost(&self) -> bool {
        self.lost.load(Ordering::SeqCst)
    }

    /// Reports a lost hook to the host once (counted; a posted message, nothing blocks).
    fn report_lost(&self) {
        if self.lost.swap(true, Ordering::SeqCst) {
            return;
        }
        self.stats.hook_timeouts.fetch_add(1, Ordering::Relaxed);
        let hwnd = self.host_window.load(Ordering::SeqCst);
        if hwnd != 0 {
            // SAFETY: posts a plain message to the host window (harmless once it is gone).
            unsafe { PostMessageW(hwnd as HWND, WM_HOOK_LOST, 0, 0) };
        }
    }

    pub fn state(&self) -> u8 {
        self.state.load(Ordering::SeqCst)
    }

    fn set_flag(&self, bit: u32, on: bool) {
        if on {
            self.flags.fetch_or(bit, Ordering::SeqCst);
        } else {
            self.flags.fetch_and(!bit, Ordering::SeqCst);
        }
    }

    pub fn set_secure_desktop(&self, on: bool) {
        self.set_flag(F_SECURE, on);
    }

    pub fn set_password_field(&self, on: bool) {
        self.set_flag(F_PASSWORD, on);
    }

    pub fn set_ime_composing(&self, on: bool) {
        self.set_flag(F_IME, on);
    }

    pub fn set_elevated_foreground(&self, on: bool) {
        self.set_flag(F_ELEVATED, on);
    }

    /// Asks the hook thread to forget the key state and to release every output it holds: input
    /// went to another desktop (lock screen, UAC, Ctrl+Alt+Del) or to an elevated window, so
    /// key-ups the hook never saw would leave stale modifiers or a remap's target held.
    pub fn request_key_reset(&self) {
        let id = self.thread_id.load(Ordering::SeqCst);
        if id != 0 {
            // SAFETY: posts a message without pointers; fails harmlessly for an ended thread.
            unsafe { PostThreadMessageW(id, WM_RESET, 0, 0) };
        }
    }

    /// Focus moved (cancels a pending sequence).
    pub fn bump_focus(&self) {
        self.focus_epoch.fetch_add(1, Ordering::SeqCst);
    }

    /// The flags the callback sees.
    pub fn flags(&self) -> Flags {
        let f = self.flags.load(Ordering::SeqCst);
        Flags {
            secure_desktop: f & F_SECURE != 0,
            password_field: f & F_PASSWORD != 0,
            ime_composing: f & F_IME != 0,
            elevated_foreground: f & F_ELEVATED != 0,
            focus_epoch: self.focus_epoch.load(Ordering::SeqCst),
        }
    }
}

/// Options of one hook.
#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    /// Never consume anything (tests, the debug test host).
    pub observe_only: bool,
    /// End the hook after this long (tests: a hook never outlives its test).
    pub max_lifetime: Option<Duration>,
}

struct Ctx {
    core: Core,
    shared: Arc<Shared>,
    queue: Queue,
    generation: u32,
    observe_only: bool,
    ticks_per_us: i64,
    /// The armed dual-role timer, if any.
    timer: usize,
}

impl Ctx {
    fn send(&self, d: &Decision) {
        for signal in d.signals() {
            self.queue.signal(self.generation, signal);
        }
    }

    fn send_all(&self, signals: &[Signal]) {
        for s in signals {
            self.queue.signal(self.generation, *s);
        }
    }

    /// Arms the one-shot dual-role timer (replacing an armed one).
    fn arm(&mut self, ms: u32) {
        // SAFETY: a thread timer (no window) of this thread; replaced or killed below.
        self.timer = unsafe { SetTimer(std::ptr::null_mut(), self.timer, ms.max(10), None) };
    }

    fn disarm(&mut self) {
        if self.timer != 0 {
            // SAFETY: a timer this thread set.
            unsafe { KillTimer(std::ptr::null_mut(), self.timer) };
            self.timer = 0;
        }
    }

    /// Arms the timer for whatever the core waits for next (an undecided dual role, the end of
    /// a timed mode), or leaves it off: an idle engine has no timer.
    fn rearm(&mut self) {
        // SAFETY: no preconditions.
        let now = unsafe { GetTickCount() };
        match self.core.next_timer(now) {
            Some(ms) => self.arm(ms),
            None => self.disarm(),
        }
    }

    /// Runs a mode request on this thread (Step 72): the state changes exactly as a mode rule
    /// would change it, and the worker gets the same signal (releases, activity, the host).
    fn mode(&mut self, req: ModeRequest) -> ModeResult {
        let from = self.core.mode_status().active;
        // SAFETY: no preconditions.
        let now = unsafe { GetTickCount() };
        let modes = self.core.table().modes().clone();
        let index = |id: &str| {
            modes.index(id).ok_or((
                "mode_not_found",
                format!("no mode `{id}` in the active profile"),
            ))
        };
        let signal = match req {
            ModeRequest::Query => None,
            ModeRequest::Set { mode, origin } => {
                let a = ModeAction::Set(index(&mode)?);
                self.core.request_mode(&a, origin, now)
            }
            ModeRequest::Toggle { mode, origin } => {
                let a = ModeAction::Toggle(index(&mode)?);
                self.core.request_mode(&a, origin, now)
            }
            ModeRequest::Reset { origin } => self.core.reset_mode(origin),
            ModeRequest::Rule {
                binding,
                generation,
            } => {
                let action = (generation == self.generation)
                    .then(|| self.core.table().mode_action(usize::from(binding)).cloned())
                    .flatten();
                match action {
                    Some(a) => self.core.request_mode(&a, ModeOrigin::Rule(binding), now),
                    None => None,
                }
            }
        };
        if let Some(s) = signal {
            self.queue.signal(self.generation, s);
        }
        self.rearm();
        let status = self.core.mode_status();
        Ok(ModeReply {
            changed: status.active != from,
            from,
            status,
        })
    }
}

thread_local! {
    static CTX: RefCell<Option<Ctx>> = const { RefCell::new(None) };
}

fn qpc() -> i64 {
    let mut t = 0i64;
    // SAFETY: `t` is a writable i64; cannot fail on Windows XP and later (documented).
    unsafe { QueryPerformanceCounter(&raw mut t) };
    t
}

/// The callback body: returns whether to consume the event.
fn handle(raw: &RawKey) -> bool {
    let start = qpc();
    let mut ticks_per_us = 1;
    let mut shared: Option<Arc<Shared>> = None;
    let consume = CTX.with(|cell| {
        // A re-entrant call (should not happen) passes the event on.
        let Ok(mut guard) = cell.try_borrow_mut() else {
            return false;
        };
        let Some(ctx) = guard.as_mut() else {
            return false;
        };
        ticks_per_us = ctx.ticks_per_us;
        shared = Some(Arc::clone(&ctx.shared));
        let state = ctx.shared.state();
        if state == STATE_OFF {
            return false;
        }
        let mode = if state == STATE_PAUSED {
            Mode::Paused
        } else {
            Mode::Running
        };
        let flags = ctx.shared.flags();
        let core = &mut ctx.core;
        let decided = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            core.decide(raw, mode, &flags)
        }));
        let Ok(d) = decided else {
            ctx.shared.stats.panics.fetch_add(1, Ordering::Relaxed);
            return false;
        };
        {
            let st = &ctx.shared.stats;
            match d.source {
                Source::Own => st.own_injected.fetch_add(1, Ordering::Relaxed),
                Source::Other => st.other_injected.fetch_add(1, Ordering::Relaxed),
                Source::Physical => st.physical.fetch_add(1, Ordering::Relaxed),
            };
        }
        if d.signal == Some(Signal::Emergency) {
            // Nothing else fires from now on; the hook goes away right after this call.
            ctx.shared.set_state(STATE_OFF);
            // SAFETY: posts a message without pointers to this very thread.
            unsafe { PostThreadMessageW(GetCurrentThreadId(), WM_UNHOOK, 0, 0) };
        }
        ctx.send(&d);
        if let Some(ms) = d.timer {
            ctx.arm(ms);
        }
        let consume = d.consume && !ctx.observe_only;
        if consume {
            ctx.shared.stats.consumed.fetch_add(1, Ordering::Relaxed);
        }
        consume
    });
    if let Some(sh) = shared {
        let st = &sh.stats;
        let us = u64::try_from((qpc() - start) / ticks_per_us.max(1)).unwrap_or(0);
        st.record_callback(us);
        // SAFETY: no preconditions.
        let now = unsafe { GetTickCount() };
        let delay = u64::from(now.wrapping_sub(raw.time_ms)).min(3_600_000);
        st.record_delay(delay);
        st.last_callback_tick.store(now, Ordering::Relaxed);
        // Step 73: Windows removes a hook that does not answer within `LowLevelHooksTimeout`
        // without telling anyone; a callback (or a physical event's wait) that long means the
        // hook may be gone, so the host turns the engine off and says so.
        if crate::recovery::hook_lost(us, delay, !raw.injected, sh.hook_timeout_us()) {
            sh.report_lost();
        }
    }
    consume
}

/// The hook procedure.
unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && lparam != 0 {
        // SAFETY: for HC_ACTION, `lparam` points to a KBDLLHOOKSTRUCT that is valid for the
        // duration of this call (documented for LowLevelKeyboardProc).
        let kb = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
        let raw = RawKey {
            vk: u16::try_from(kb.vkCode).unwrap_or(0),
            scan: u16::try_from(kb.scanCode).unwrap_or(0),
            extended: kb.flags & LLKHF_EXTENDED != 0,
            up: kb.flags & LLKHF_UP != 0,
            injected: kb.flags & LLKHF_INJECTED != 0,
            lower_il_injected: kb.flags & LLKHF_LOWER_IL_INJECTED != 0,
            extra_info: kb.dwExtraInfo,
            time_ms: kb.time,
        };
        if handle(&raw) {
            return 1; // consumed: the event goes no further
        }
    }
    // SAFETY: passes the event to the next hook; the hook handle parameter is ignored.
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

/// The middle and X button events as the core sees them (`VK_*BUTTON`, down or up); every other
/// mouse message is `None` and passes on untouched.
fn mouse_button(message: u32, mouse_data: u32) -> Option<(Button, bool)> {
    let x = || match u16::try_from(mouse_data >> 16).unwrap_or(0) {
        XBUTTON1 => Some(Button::X1),
        XBUTTON2 => Some(Button::X2),
        _ => None,
    };
    match message {
        WM_MBUTTONDOWN => Some((Button::Middle, false)),
        WM_MBUTTONUP => Some((Button::Middle, true)),
        WM_XBUTTONDOWN => x().map(|b| (b, false)),
        WM_XBUTTONUP => x().map(|b| (b, true)),
        _ => None,
    }
}

/// The mouse hook procedure: moves, wheel and primary/secondary buttons pass on at once.
unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && lparam != 0 {
        let message = u32::try_from(wparam).unwrap_or(0);
        if matches!(
            message,
            WM_MBUTTONDOWN | WM_MBUTTONUP | WM_XBUTTONDOWN | WM_XBUTTONUP
        ) {
            // SAFETY: for HC_ACTION, `lparam` points to an MSLLHOOKSTRUCT that is valid for the
            // duration of this call (documented for LowLevelMouseProc).
            let ms = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
            if let Some((b, up)) = mouse_button(message, ms.mouseData) {
                let raw = RawKey {
                    vk: b.vk(),
                    scan: 0,
                    extended: false,
                    up,
                    injected: ms.flags & LLMHF_INJECTED != 0,
                    lower_il_injected: ms.flags & LLMHF_LOWER_IL_INJECTED != 0,
                    extra_info: ms.dwExtraInfo,
                    time_ms: ms.time,
                };
                if handle(&raw) {
                    return 1;
                }
            }
        }
    }
    // SAFETY: passes the event to the next hook; the hook handle parameter is ignored.
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

/// Unhooks on drop, including while unwinding.
struct Installed(HHOOK, Arc<Shared>);

impl Drop for Installed {
    fn drop(&mut self) {
        // SAFETY: a hook this thread installed, removed exactly once.
        unsafe { UnhookWindowsHookEx(self.0) };
        self.1.installed.store(false, Ordering::SeqCst);
    }
}

/// The mouse hook; unhooks on drop, including while unwinding.
struct MouseHook(HHOOK, Arc<Shared>);

impl MouseHook {
    fn install(shared: &Arc<Shared>) -> Option<MouseHook> {
        // SAFETY: as for the keyboard hook: this executable's module handle, the documented
        // procedure signature, thread id 0 (required for low-level hooks; no DLL is injected).
        let hook = unsafe {
            SetWindowsHookExW(
                WH_MOUSE_LL,
                Some(mouse_proc),
                GetModuleHandleW(std::ptr::null()),
                0,
            )
        };
        if hook.is_null() {
            return None;
        }
        shared.mouse_installed.store(true, Ordering::SeqCst);
        Some(MouseHook(hook, Arc::clone(shared)))
    }
}

impl Drop for MouseHook {
    fn drop(&mut self) {
        // SAFETY: a hook this thread installed, removed exactly once.
        unsafe { UnhookWindowsHookEx(self.0) };
        self.1.mouse_installed.store(false, Ordering::SeqCst);
    }
}

/// Posts `WM_QUIT` to a hook thread from anywhere (watchdogs, the host).
#[derive(Clone, Copy, Debug)]
pub struct QuitHandle(u32);

impl QuitHandle {
    /// Ends the hook thread; harmless when it already ended.
    pub fn quit(self) {
        // SAFETY: posts a message without pointers; fails harmlessly for an ended thread.
        unsafe { PostThreadMessageW(self.0, WM_QUIT, 0, 0) };
    }
}

/// The running hook thread.
#[derive(Debug)]
pub struct HookThread {
    thread_id: u32,
    join: Option<std::thread::JoinHandle<()>>,
    shared: Arc<Shared>,
    watchdog: Option<Sender<()>>,
}

impl HookThread {
    /// Installs the hook on a new thread with `table`. Fails when Windows refuses the hook.
    pub fn start(
        table: Table,
        generation: u32,
        shared: &Arc<Shared>,
        queue: Queue,
        opts: Options,
    ) -> io::Result<HookThread> {
        let (ready_tx, ready_rx) = channel::<io::Result<u32>>();
        let sh = Arc::clone(shared);
        let join = std::thread::Builder::new()
            .name("ddalgak-hook".into())
            .spawn(move || run(table, generation, &sh, queue, opts.observe_only, &ready_tx))?;
        let thread_id = match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(id)) => id,
            Ok(Err(e)) => {
                let _ = join.join();
                return Err(e);
            }
            Err(_) => return Err(io::Error::other("the hook thread did not start")),
        };
        shared.thread_id.store(thread_id, Ordering::SeqCst);
        let watchdog = opts.max_lifetime.map(|limit| {
            let (tx, rx) = channel::<()>();
            let quit = QuitHandle(thread_id);
            std::thread::spawn(move || {
                if rx.recv_timeout(limit).is_err() {
                    quit.quit();
                }
            });
            tx
        });
        Ok(HookThread {
            thread_id,
            join: Some(join),
            shared: Arc::clone(shared),
            watchdog,
        })
    }

    pub fn quit_handle(&self) -> QuitHandle {
        QuitHandle(self.thread_id)
    }

    /// Hands a new table to the callback (a reload). The swap happens between callbacks.
    pub fn swap_table(&self, generation: u32, table: Table) {
        if let Ok(mut p) = self.shared.pending.lock() {
            *p = Some((generation, table));
        }
        // SAFETY: posts a message without pointers to the hook thread.
        unsafe { PostThreadMessageW(self.thread_id, WM_SWAP, 0, 0) };
    }

    /// Hands the rules of another profile to the callback (Step 75, a profile switch): between
    /// two callbacks the old profile's pending and held work ends and the new rules start in their
    /// default mode ([`crate::core::Core::switch_table`]).
    pub fn switch_table(&self, generation: u32, table: Table) {
        if let Ok(mut p) = self.shared.pending.lock() {
            *p = Some((generation, table));
        }
        // SAFETY: posts a message without pointers to the hook thread.
        unsafe { PostThreadMessageW(self.thread_id, WM_SWAP, 1, 0) };
    }

    /// Drops pending taps, holds and sequences (pause, session lock).
    pub fn cancel_pending(&self) {
        // SAFETY: posts a message without pointers to the hook thread.
        unsafe { PostThreadMessageW(self.thread_id, WM_CANCEL, 0, 0) };
    }

    /// Like [`HookThread::cancel_pending`], and the mode returns to the default mode (a pause:
    /// the safe reset, Step 72).
    pub fn cancel_and_reset_mode(&self) {
        // SAFETY: posts a message without pointers to the hook thread.
        unsafe { PostThreadMessageW(self.thread_id, WM_CANCEL, 1, 0) };
    }

    /// Asks the hook thread, which owns the mode state, to read or change it (Step 72). Waits at
    /// most one second; `None` when the thread did not answer.
    pub fn mode(&self, req: ModeRequest) -> Option<ModeResult> {
        let (tx, rx) = channel();
        if let Ok(mut m) = self.shared.mode_req.lock() {
            *m = Some((req, tx));
        }
        // SAFETY: posts a message without pointers to the hook thread.
        unsafe { PostThreadMessageW(self.thread_id, WM_MODE, 0, 0) };
        rx.recv_timeout(Duration::from_secs(1)).ok()
    }

    /// Whether the thread still runs and its hook is installed.
    pub fn alive(&self) -> bool {
        self.join.as_ref().is_some_and(|j| !j.is_finished())
            && self.shared.installed.load(Ordering::SeqCst)
    }

    /// Removes the hook and ends the thread.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        if let Some(w) = self.watchdog.take() {
            let _ = w.send(());
        }
        self.quit_handle().quit();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for HookThread {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run(
    table: Table,
    generation: u32,
    shared: &Arc<Shared>,
    queue: Queue,
    observe_only: bool,
    ready: &Sender<io::Result<u32>>,
) {
    // SAFETY: MSG is plain data; all-zero is a valid out-parameter value.
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    // SAFETY: creates this thread's message queue before anyone posts to it.
    unsafe { PeekMessageW(&raw mut msg, std::ptr::null_mut(), 0, 0, PM_NOREMOVE) };
    let mut freq = 0i64;
    // SAFETY: `freq` is a writable i64.
    unsafe { QueryPerformanceFrequency(&raw mut freq) };
    let wants_mouse = table.has_mouse();
    CTX.with(|c| {
        *c.borrow_mut() = Some(Ctx {
            core: {
                let mut core = Core::new(table);
                // Typing runs read the modifier state and the key count; hotstrings translate
                // keys without changing keyboard state (Step 69, ADR 0034).
                core.set_runs(queue.runs());
                core.set_translator(super::typing::translate);
                core
            },
            shared: Arc::clone(shared),
            queue,
            generation,
            observe_only,
            ticks_per_us: (freq / 1_000_000).max(1),
            timer: 0,
        });
    });
    // SAFETY: the module handle of this executable; a low-level hook needs one but is called in
    // this thread's context (no DLL is injected anywhere, documented).
    let module = unsafe { GetModuleHandleW(std::ptr::null()) };
    // SAFETY: `hook_proc` has the documented signature; thread id 0 = all threads (required for
    // low-level hooks).
    let hook = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), module, 0) };
    if hook.is_null() {
        let _ = ready.send(Err(io::Error::last_os_error()));
        CTX.with(|c| c.borrow_mut().take());
        return;
    }
    shared.installed.store(true, Ordering::SeqCst);
    // A fresh hook: a loss of the previous one is history (counted in the stats).
    shared.lost.store(false, Ordering::SeqCst);
    let installed = Installed(hook, Arc::clone(shared));
    // The mouse hook only while a mouse-button rule exists (installed after the keyboard hook;
    // a refusal leaves the keyboard rules working and is visible in `health`).
    let mut mouse = if wants_mouse {
        MouseHook::install(shared)
    } else {
        None
    };
    // SAFETY: no preconditions.
    let _ = ready.send(Ok(unsafe { GetCurrentThreadId() }));
    loop {
        // SAFETY: `msg` is a valid out-pointer; null window = this thread's messages. The hook
        // procedure runs inside this call. Returns 0 for WM_QUIT and -1 on error.
        let r = unsafe { GetMessageW(&raw mut msg, std::ptr::null_mut(), 0, 0) };
        if r == 0 || r == -1 {
            break;
        }
        shared.stats.hook_wakeups.fetch_add(1, Ordering::Relaxed);
        match msg.message {
            WM_UNHOOK => break,
            WM_SWAP => {
                let switch = msg.wParam == 1;
                let next = shared.pending.lock().ok().and_then(|mut p| p.take());
                if let Some((generation, table)) = next {
                    let wants_mouse = table.has_mouse();
                    CTX.with(|c| {
                        if let Some(ctx) = c.borrow_mut().as_mut() {
                            let mode = if switch {
                                // A profile switch: the old rules' work ends under their own
                                // generation, then the new rules start in their default mode.
                                let (before, mode) = ctx.core.switch_table(table);
                                ctx.send_all(&before);
                                mode
                            } else {
                                ctx.core.set_table(table)
                            };
                            ctx.generation = generation;
                            ctx.disarm();
                            // The worker and the host learn the modes of the new rules.
                            ctx.queue.signal(generation, mode);
                            ctx.rearm();
                        }
                    });
                    match (wants_mouse, mouse.is_some()) {
                        (true, false) => mouse = MouseHook::install(shared),
                        (false, true) => mouse = None,
                        _ => {}
                    }
                }
            }
            WM_RESET => {
                CTX.with(|c| {
                    if let Some(ctx) = c.borrow_mut().as_mut() {
                        ctx.disarm();
                        let signals = ctx.core.reset_keys();
                        ctx.send_all(&signals);
                        ctx.rearm();
                    }
                });
            }
            WM_CANCEL => {
                let reset = msg.wParam == 1;
                CTX.with(|c| {
                    if let Some(ctx) = c.borrow_mut().as_mut() {
                        ctx.disarm();
                        let signals = ctx.core.cancel_pending();
                        ctx.send_all(&signals);
                        if reset && let Some(s) = ctx.core.reset_mode(ModeOrigin::Pause) {
                            ctx.queue.signal(ctx.generation, s);
                        }
                        ctx.rearm();
                    }
                });
            }
            WM_MODE => {
                let req = shared.mode_req.lock().ok().and_then(|mut m| m.take());
                if let Some((req, reply)) = req {
                    let answer = CTX.with(|c| {
                        c.borrow_mut()
                            .as_mut()
                            .map(|ctx| ctx.mode(req))
                            .unwrap_or(Err(("engine_off", "the engine is off".into())))
                    });
                    let _ = reply.send(answer);
                }
            }
            WM_TIMER if msg.hwnd.is_null() => {
                CTX.with(|c| {
                    if let Some(ctx) = c.borrow_mut().as_mut()
                        && ctx.timer != 0
                        && msg.wParam == ctx.timer
                    {
                        ctx.disarm();
                        // SAFETY: no preconditions.
                        let now = unsafe { GetTickCount() };
                        let (signal, _) = ctx.core.tick(now);
                        if let Some(s) = signal {
                            ctx.queue.signal(ctx.generation, s);
                        }
                        // A mode with a timeout that saw no key for that long ends (Step 72).
                        if let Some(s) = ctx.core.mode_tick(now) {
                            ctx.queue.signal(ctx.generation, s);
                        }
                        ctx.rearm();
                    }
                });
            }
            _ => {}
        }
    }
    drop(mouse);
    drop(installed);
    CTX.with(|c| {
        if let Some(mut ctx) = c.borrow_mut().take() {
            ctx.disarm();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_middle_and_x_buttons_reach_the_core() {
        assert_eq!(
            mouse_button(WM_MBUTTONDOWN, 0),
            Some((Button::Middle, false))
        );
        assert_eq!(
            mouse_button(WM_XBUTTONUP, u32::from(XBUTTON2) << 16),
            Some((Button::X2, true))
        );
        assert_eq!(
            mouse_button(WM_XBUTTONDOWN, u32::from(XBUTTON1) << 16),
            Some((Button::X1, false))
        );
        assert_eq!(mouse_button(WM_XBUTTONDOWN, 7 << 16), None);
        for other in [0x0200u32, 0x0201, 0x0202, 0x0204, 0x0205, 0x020A, 0x020E] {
            assert_eq!(mouse_button(other, 0), None, "message {other:#x} passes on");
        }
    }
}
