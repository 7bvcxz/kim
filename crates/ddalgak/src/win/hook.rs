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

use crate::core::{Core, Flags, Mode, Signal};
use crate::event::{RawKey, Source};
use crate::stats::Stats;
use crate::table::Table;
use crate::worker::Queue;
use std::cell::RefCell;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows_sys::Win32::System::SystemInformation::GetTickCount;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_INJECTED,
    LLKHF_LOWER_IL_INJECTED, LLKHF_UP, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW,
    SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_APP, WM_QUIT,
};

/// Engine state as the callback reads it.
pub const STATE_OFF: u8 = 0;
pub const STATE_RUNNING: u8 = 1;
pub const STATE_PAUSED: u8 = 2;

const WM_SWAP: u32 = WM_APP + 0x10;
const WM_UNHOOK: u32 = WM_APP + 0x11;
const WM_CANCEL: u32 = WM_APP + 0x12;
const WM_RESET: u32 = WM_APP + 0x13;

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
    /// The hook thread, for key-state resets requested by the watchers.
    thread_id: AtomicU32,
    pub stats: Arc<Stats>,
    pending: Mutex<Option<(u32, Table)>>,
}

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

    /// Asks the hook thread to forget the key state: input went to another desktop (lock
    /// screen, UAC, Ctrl+Alt+Del), so key-ups the hook never saw would leave stale modifiers.
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
fn handle(kb: &KBDLLHOOKSTRUCT) -> bool {
    let start = qpc();
    let mut ticks_per_us = 1;
    let mut stats: Option<Arc<Stats>> = None;
    let consume = CTX.with(|cell| {
        // A re-entrant call (should not happen) passes the event on.
        let Ok(mut guard) = cell.try_borrow_mut() else {
            return false;
        };
        let Some(ctx) = guard.as_mut() else {
            return false;
        };
        ticks_per_us = ctx.ticks_per_us;
        stats = Some(Arc::clone(&ctx.shared.stats));
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
        let core = &mut ctx.core;
        let decided = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            core.decide(&raw, mode, &flags)
        }));
        let Ok(d) = decided else {
            ctx.shared.stats.panics.fetch_add(1, Ordering::Relaxed);
            return false;
        };
        let st = &ctx.shared.stats;
        match d.source {
            Source::Own => st.own_injected.fetch_add(1, Ordering::Relaxed),
            Source::Other => st.other_injected.fetch_add(1, Ordering::Relaxed),
            Source::Physical => 0,
        };
        if let Some(signal) = d.signal {
            if signal == Signal::Emergency {
                // Nothing else fires from now on; the hook goes away right after this call.
                ctx.shared.set_state(STATE_OFF);
                // SAFETY: posts a message without pointers to this very thread.
                unsafe { PostThreadMessageW(GetCurrentThreadId(), WM_UNHOOK, 0, 0) };
            }
            ctx.queue.signal(ctx.generation, signal);
        }
        let consume = d.consume && !ctx.observe_only;
        if consume {
            st.consumed.fetch_add(1, Ordering::Relaxed);
        }
        consume
    });
    if let Some(st) = stats {
        let us = (qpc() - start) / ticks_per_us.max(1);
        st.record_callback(u64::try_from(us).unwrap_or(0));
        // SAFETY: no preconditions.
        let now = unsafe { GetTickCount() };
        st.record_delay(u64::from(now.wrapping_sub(kb.time)).min(3_600_000));
        st.last_callback_tick.store(now, Ordering::Relaxed);
    }
    consume
}

/// The hook procedure.
unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && lparam != 0 {
        // SAFETY: for HC_ACTION, `lparam` points to a KBDLLHOOKSTRUCT that is valid for the
        // duration of this call (documented for LowLevelKeyboardProc).
        let kb = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
        if handle(kb) {
            return 1; // consumed: the event goes no further
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

    /// Drops pending taps, holds and sequences (pause, session lock).
    pub fn cancel_pending(&self) {
        // SAFETY: posts a message without pointers to the hook thread.
        unsafe { PostThreadMessageW(self.thread_id, WM_CANCEL, 0, 0) };
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
    CTX.with(|c| {
        *c.borrow_mut() = Some(Ctx {
            core: Core::new(table),
            shared: Arc::clone(shared),
            queue,
            generation,
            observe_only,
            ticks_per_us: (freq / 1_000_000).max(1),
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
    let installed = Installed(hook, Arc::clone(shared));
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
                let next = shared.pending.lock().ok().and_then(|mut p| p.take());
                if let Some((generation, table)) = next {
                    CTX.with(|c| {
                        if let Some(ctx) = c.borrow_mut().as_mut() {
                            ctx.core.set_table(table);
                            ctx.generation = generation;
                        }
                    });
                }
            }
            WM_RESET => {
                CTX.with(|c| {
                    if let Some(ctx) = c.borrow_mut().as_mut() {
                        ctx.core.reset_keys();
                    }
                });
            }
            WM_CANCEL => {
                CTX.with(|c| {
                    if let Some(ctx) = c.borrow_mut().as_mut()
                        && let Some(s) = ctx.core.cancel_pending()
                    {
                        ctx.queue.signal(ctx.generation, s);
                    }
                });
            }
            _ => {}
        }
    }
    drop(installed);
    CTX.with(|c| c.borrow_mut().take());
}
