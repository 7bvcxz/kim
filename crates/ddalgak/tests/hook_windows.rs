//! The real `WH_KEYBOARD_LL` hook on Windows (Step 66, ADR 0024): install and clean-up on every
//! path, the idle benchmark (wake-ups and CPU), and — when an interactive desktop is available —
//! injected-event loop prevention, the injected-input policy, held-key release and callback
//! latency with real `SendInput`.
//!
//! Machine safety (the maintainer uses this PC): the hook is **observe-only** (every event goes to
//! `CallNextHookEx`, nothing is consumed), it acts only on the obscure keys F13–F17 this test
//! injects, it has a 10 s watchdog besides `Drop` and the explicit stop, and the test asserts
//! only counts of its own events and the state of its own keys. On a locked workstation (no
//! interactive input desktop) the injection part reports **skipped (locked)**.
//!
//! Step 68 adds a real-input smoke test of remaps and blocks
//! (`real_remap_block_and_dual_role_with_obscure_keys`). Its hook consumes keys, but only the
//! test's own obscure keys F15, F17 and F18 (every other event goes to `CallNextHookEx`), and it
//! injects only F16, F19 and F20. Remaps never accept injected input in a rules file (schema
//! `recursion_risk`); `SendInput` always marks input as injected, so the test builds its bindings
//! directly with `injected: accept` to stand in for a physical key. Results are read from
//! `GetAsyncKeyState` of those keys and the engine's own counters, never from a focused
//! application. No mouse input is injected; mouse-button remaps are covered by the core tests.
//!
//! Step 69 adds a smoke test of the typing-run path (`a_send_keys_run_reaches_send_input_with_an_
//! obscure_key_only`): a `send_keys` run taps only `F16` through the real `SendInput`; no
//! character is ever typed and the clipboard is never touched.
//!
//! Step 71 adds a smoke test of the mouse trigger path (`mouse_rules_on_obscure_keys_reach_only_
//! a_simulated_pointer`): real injected F13–F15 drive a held button, a glide and a scroll whose
//! output goes to a simulated pointer; **no mouse input is ever sent** (the maintainer uses this
//! machine's pointer).
//!
//! Step 75 makes every real-input step robust against the person using this desktop at the same
//! time (`common/quiet.rs`): a step whose result physical input can change (a held modifier keeps
//! an exact trigger from matching; any key going down decides a dual role) runs as attempts judged
//! with the hook's own count of physical events, and only an attempt without physical input may
//! fail the test. Key states are awaited (`eventually`), never read once right after a counter
//! moved: the hook counts an event before Windows updates the async key state for it.
//!
//! Requirements: `DDALGAK-NFR-001`, `DDALGAK-NFR-002`, `DDALGAK-SEC-004`, `DDALGAK-SEC-005`,
//! `DDALGAK-FR-002`, `DDALGAK-FR-010`, `DDALGAK-FR-011`.
#![cfg(windows)]

#[path = "common/quiet.rs"]
mod quiet;

use ddalgak::inject::{Injector, OutKey};
use ddalgak::remap::Output;
use ddalgak::stats::Stats;
use ddalgak::table::{Action, Binding, EngineOp, Injected, On, RemapSpec, Table};
use ddalgak::trigger::Trigger;
use ddalgak::win::hook::{self, HookThread, STATE_RUNNING, Shared};
use ddalgak::win::inject::{SendInputBackend, send_as_other_program};
use ddalgak::worker::{Activity, Control, Msg, Rules, Worker};
use quiet::{Fail, Outcome, check, eventually, sent};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

const VK_F13: u16 = 0x7C;
const VK_F14: u16 = 0x7D;
const VK_F15: u16 = 0x7E;
const VK_F16: u16 = 0x7F;
const VK_F17: u16 = 0x80;
const VK_F18: u16 = 0x81;
const VK_F19: u16 = 0x82;
const VK_F20: u16 = 0x83;

fn binding(rule: &str, keys: &str, injected: Injected, op: EngineOp) -> Binding {
    Binding {
        rule: rule.into(),
        trigger: Trigger::parse(keys).unwrap(),
        on: On::Press,
        repeat: false,
        pass_through: false,
        injected,
        widen: false,
        injects: false,
        action: Action::Engine(vec![op]),
        hotstring: None,
        modes: 0,
    }
}

struct Engine {
    stats: Arc<Stats>,
    shared: Arc<Shared>,
    worker: Worker,
    control: Receiver<Control>,
    hook: Option<HookThread>,
}

fn engine(bindings: Vec<Binding>, lifetime: Duration) -> Engine {
    engine_with(bindings, lifetime, true)
}

fn engine_with(bindings: Vec<Binding>, lifetime: Duration, observe_only: bool) -> Engine {
    let stats = Arc::new(Stats::default());
    let shared = Shared::new(Arc::clone(&stats));
    let (tx, control) = channel();
    let tx = Mutex::new(tx);
    let worker = Worker::start(
        Arc::clone(&stats),
        Box::new(move |c| {
            let _ = tx.lock().unwrap().send(c);
        }),
        Arc::new(Mutex::new(Activity::default())),
        Box::new(SendInputBackend),
        ddalgak::run::Env::inert(),
    );
    worker.queue().send(Msg::Rules(Rules {
        generation: 1,
        bindings: Arc::new(bindings.clone()),
        profile: "default".into(),
        mode: "base".into(),
        modes: ddalgak::modes::ModeSet::single(),
    }));
    shared.set_state(STATE_RUNNING);
    let t = Instant::now();
    let hook = HookThread::start(
        Table::compile(bindings, 20),
        1,
        &shared,
        worker.queue(),
        hook::Options {
            observe_only,
            max_lifetime: Some(lifetime),
        },
    )
    .expect("Windows installs a low-level keyboard hook for a normal user process");
    eprintln!("hook installed in {:?}", t.elapsed());
    Engine {
        stats,
        shared,
        worker,
        control,
        hook: Some(hook),
    }
}

#[allow(unsafe_code)]
fn key_down(vk: u16) -> bool {
    // SAFETY: reads the async state of one of this test's own obscure keys.
    (unsafe { GetAsyncKeyState(i32::from(vk)) } as u16) & 0x8000 != 0
}

fn tap_other(vk: u16) -> usize {
    send_as_other_program(&[OutKey { vk, up: false }, OutKey { vk, up: true }])
}

/// The tests inject into the whole session and watch the same obscure keys: they take turns.
static SERIAL: Mutex<()> = Mutex::new(());

/// A turn of a real-input test: this process's lock and a session-wide named mutex, so the same
/// tests running at the same time from another checkout (parallel builds on this machine) never
/// inject into each other's hooks (Step 73: that is what made a dual role resolve early once).
struct Turn {
    _local: std::sync::MutexGuard<'static, ()>,
    session: windows_sys::Win32::Foundation::HANDLE,
}

impl Drop for Turn {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::ReleaseMutex;
        if !self.session.is_null() {
            // SAFETY: a mutex this thread owns (acquired in `serial`), released and closed once.
            unsafe {
                ReleaseMutex(self.session);
                CloseHandle(self.session);
            }
        }
    }
}

#[allow(unsafe_code)]
fn serial() -> Turn {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_ABANDONED, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};
    let local = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let name = ddalgak::win::wide("Local\\mouv-ddalgak-real-input-tests");
    // SAFETY: NUL-terminated name, default security, not initially owned.
    let h = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    let mut session = std::ptr::null_mut();
    if !h.is_null() {
        // SAFETY: a valid mutex handle; waits at most three minutes for another checkout's test.
        match unsafe { WaitForSingleObject(h, 180_000) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED => session = h,
            _ => {
                eprintln!("note: another real-input test kept its turn for 3 minutes; going ahead");
                // SAFETY: closes the handle opened above (not owned).
                unsafe { CloseHandle(h) };
            }
        }
    }
    Turn {
        _local: local,
        session,
    }
}

#[test]
fn real_hook_idle_cost_injected_cycles_held_keys_and_cleanup() {
    let _turn = serial();
    let mut e = engine(
        vec![
            binding("accept-f13", "F13", Injected::Accept, EngineOp::Pause),
            binding("ignore-f14", "F14", Injected::Ignore, EngineOp::Pause),
            binding("own-f15", "F15", Injected::Accept, EngineOp::Disable),
        ],
        Duration::from_secs(10),
    );
    assert!(e.shared.installed.load(Ordering::SeqCst));
    assert!(e.hook.as_ref().unwrap().alive());

    // --- Idle benchmark: the hook thread and the worker sleep; nothing wakes them but input.
    std::thread::sleep(Duration::from_millis(200));
    let s = &e.stats;
    let (hw0, ww0, cb0) = (
        s.hook_wakeups.load(Ordering::Relaxed),
        s.worker_wakeups.load(Ordering::Relaxed),
        s.callbacks.load(Ordering::Relaxed),
    );
    let cpu0 = ddalgak::win::process_cpu_us();
    let idle = Duration::from_secs(2);
    std::thread::sleep(idle);
    let cpu = ddalgak::win::process_cpu_us() - cpu0;
    let (hw, ww, cb) = (
        s.hook_wakeups.load(Ordering::Relaxed) - hw0,
        s.worker_wakeups.load(Ordering::Relaxed) - ww0,
        s.callbacks.load(Ordering::Relaxed) - cb0,
    );
    eprintln!(
        "idle benchmark ({idle:?}, hook installed): hook-thread wake-ups {hw}, worker wake-ups {ww}, \
         hook callbacks {cb} (other input on this desktop), test-process CPU {} µs ({:.4} % of one core)",
        cpu,
        cpu as f64 / idle.as_micros() as f64 * 100.0
    );
    assert_eq!(hw, 0, "no posted message and no timer woke the hook thread");
    assert_eq!(
        ww, 0,
        "no timer woke the worker (no rule matched any input)"
    );

    // --- Injection (needs the interactive input desktop).
    if !ddalgak::win::input_desktop_available() {
        eprintln!(
            "skipped (locked): no interactive input desktop, so SendInput and the hook cannot be \
             exercised with real input; the decision logic is covered by the core unit tests"
        );
    } else {
        injected(&mut e);
    }

    // --- Clean-up: stop removes the hook and ends the thread.
    e.hook.take().unwrap().stop();
    assert!(
        !e.shared.installed.load(Ordering::SeqCst),
        "the hook is removed"
    );

    // The watchdog removes a hook that outlives its limit (the timeout path of every test hook).
    let w = engine(vec![], Duration::from_millis(300));
    let t = Instant::now();
    while w.shared.installed.load(Ordering::SeqCst) && t.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !w.shared.installed.load(Ordering::SeqCst),
        "the watchdog unhooked"
    );
    assert!(!w.hook.as_ref().unwrap().alive());
    drop(w);

    // Drop on a panic path also unhooks (a panicking test still cleans up).
    let shared = {
        let p = engine(vec![], Duration::from_secs(10));
        let shared = Arc::clone(&p.shared);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _keep = p;
            panic!("simulated test failure");
        }));
        assert!(r.is_err());
        shared
    };
    assert!(
        !shared.installed.load(Ordering::SeqCst),
        "Drop unhooked while unwinding"
    );
    drop(e.worker);
}

/// Ends the injection part when Windows refuses input: the desktop can lock between the check
/// and any call, and UIPI refuses input while an elevated window has focus. What was verified so
/// far is printed; nothing is faked.
macro_rules! inject_or_skip {
    ($ok:expr, $step:expr, $verified:expr) => {
        if !$ok {
            eprintln!(
                "skipped (SendInput refused) at `{}`: locked desktop or an elevated window has \
                 focus; verified before: {:?}",
                $step, $verified
            );
            return;
        }
    };
}

/// Records a step run as attempts (`quiet::attempts`): verified, skipped while a person typed
/// (reported by the harness), or the end of the injection part when `SendInput` was refused.
macro_rules! step {
    ($outcome:expr, $label:expr, $verified:expr) => {
        match $outcome {
            Outcome::Verified(v) => {
                $verified.push($label);
                Some(v)
            }
            Outcome::Disturbed => None,
            Outcome::Refused(at) => {
                eprintln!(
                    "skipped (SendInput refused) at `{at}`: locked desktop or an elevated window \
                     has focus; verified before: {:?}",
                    $verified
                );
                return;
            }
        }
    };
}

fn injected(e: &mut Engine) {
    let s = Arc::clone(&e.stats);
    let physical = || s.physical.load(Ordering::Relaxed);
    let mut verified: Vec<&str> = Vec::new();
    let own0 = s.own_injected.load(Ordering::Relaxed);
    let other0 = s.other_injected.load(Ordering::Relaxed);
    // 1. Ddalgak's own input (marked) is passed on and never matched, even by an accepting rule:
    //    an injected cycle cannot re-trigger the engine. Physical input cannot change this.
    let mut inj = Injector::new(SendInputBackend);
    for _ in 0..20 {
        inject_or_skip!(inj.tap(VK_F15), "own F15 taps", verified);
    }
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        e.control.try_recv().is_err(),
        "own input never fired `own-f15`"
    );
    let own = s.own_injected.load(Ordering::Relaxed) - own0;
    assert!(own >= 40, "the hook saw its own 40 events as own: {own}");
    verified.push("own injected input is never matched (40 events)");
    // 2. Another program's input reaches only `injected: accept` rules. `F13` is an exact
    //    trigger: a modifier a person holds keeps it from matching (a disturbed attempt).
    let control = &e.control;
    let activity = &e.worker.activity;
    step!(
        quiet::attempts(
            "other programs' input reaches accept rules only",
            physical,
            |_| {
                while control.try_recv().is_ok() {}
                sent(tap_other(VK_F14) == 2, "other-program F14")?;
                sent(tap_other(VK_F13) == 2, "other-program F13")?;
                let c = control.recv_timeout(Duration::from_secs(3));
                check(
                    c == Ok(Control::Engine {
                        op: EngineOp::Pause,
                        rule: "accept-f13".into(),
                    }),
                    || format!("accept-f13 fired: {c:?}"),
                )?;
                std::thread::sleep(Duration::from_millis(200));
                assert!(control.try_recv().is_err(), "ignore-f14 did not fire");
                let entries: Vec<(String, String)> = activity
                    .lock()
                    .unwrap()
                    .entries()
                    .map(|x| (x.rule.clone(), x.outcome.clone()))
                    .collect();
                check(
                    entries.contains(&("ignore-f14".into(), "skipped_injected_input".into()))
                        && entries.contains(&("accept-f13".into(), "ok".into())),
                    || format!("{entries:?}"),
                )
            },
            || {},
        ),
        "other programs' input reaches accept rules only",
        verified
    );
    assert!(s.other_injected.load(Ordering::Relaxed) - other0 >= 4);
    assert_eq!(
        s.consumed.load(Ordering::Relaxed),
        0,
        "observe-only: nothing consumed"
    );
    // 3. Held keys are released on release_all (pause/stop/emergency paths) and on drop. The
    //    hook counts an event before Windows updates the key state, so states are awaited.
    inject_or_skip!(inj.press(VK_F16), "press F16", verified);
    assert!(
        eventually(1000, || key_down(VK_F16)),
        "F16 is logically down"
    );
    assert_eq!(inj.release_all(), 1);
    assert!(
        eventually(1000, || !key_down(VK_F16)),
        "release_all released F16"
    );
    {
        let mut scoped = Injector::new(SendInputBackend);
        inject_or_skip!(scoped.press(VK_F17), "press F17", verified);
        assert!(eventually(1000, || key_down(VK_F17)));
    }
    assert!(eventually(1000, || !key_down(VK_F17)), "Drop released F17");
    verified.push("held keys are released by release_all and on drop");
    // 4. A loop through another program is cut by the rate limit (20 per second). A held
    //    modifier keeps the storm from matching at all (disturbed); between attempts the
    //    one-second window of the limit runs out.
    let count_limited = || {
        activity
            .lock()
            .unwrap()
            .entries()
            .filter(|x| x.outcome == "rate_limited")
            .count()
    };
    step!(
        quiet::attempts(
            "a 40-event storm through another program is rate-limited",
            physical,
            |_| {
                while control.try_recv().is_ok() {}
                let limited0 = count_limited();
                for _ in 0..40 {
                    sent(tap_other(VK_F13) == 2, "F13 storm")?;
                }
                std::thread::sleep(Duration::from_millis(500));
                let fired = std::iter::from_fn(|| control.try_recv().ok()).count();
                // Physical input can only keep runs from matching, never add any: a hard limit.
                assert!(fired <= 20, "at most max_runs_per_second runs: {fired}");
                let limited = count_limited() - limited0;
                check(limited >= 19, || {
                    format!("rate_limited outcomes recorded: {limited} (runs {fired})")
                })
            },
            || std::thread::sleep(Duration::from_millis(1100)),
        ),
        "a 40-event storm through another program is rate-limited",
        verified
    );
    // 5. Callback latency (DDALGAK-NFR-002: p99 < 1 ms).
    let p99 = s.p99_us().unwrap();
    eprintln!(
        "hook callbacks: {} total ({} physical), max {} µs, p99 ≤ {} µs, max event delay {} ms",
        s.callbacks.load(Ordering::Relaxed),
        s.physical.load(Ordering::Relaxed),
        s.max_us.load(Ordering::Relaxed),
        p99,
        s.max_delay_ms.load(Ordering::Relaxed)
    );
    assert!(p99 <= 1_000, "p99 callback time {p99} µs");
    verified.push("p99 callback time below 1 ms");
    eprintln!("verified with real SendInput: {verified:?}");
}

/// A remap binding that other programs' input may trigger (test only: see the module comment).
fn remap_binding(rule: &str, keys: &str, to: &str, tap: Option<&str>) -> Binding {
    Binding {
        rule: rule.into(),
        trigger: Trigger::parse(keys).unwrap(),
        on: On::Press,
        repeat: false,
        pass_through: false,
        injected: Injected::Accept,
        widen: true,
        injects: true,
        action: Action::Remap(RemapSpec {
            to: Output::parse(to).unwrap(),
            tap: tap.map(|t| Output::parse(t).unwrap()),
            tap_ms: 200,
        }),
        hotstring: None,
        modes: 0,
    }
}

fn send_other(vk: u16, up: bool) -> bool {
    send_as_other_program(&[OutKey { vk, up }]) == 1
}

#[test]
fn real_remap_block_and_dual_role_with_obscure_keys() {
    let _turn = serial();
    if !ddalgak::win::input_desktop_available() {
        eprintln!(
            "skipped (locked): no interactive input desktop; remaps are covered by the core and \
             worker unit tests"
        );
        return;
    }
    let mut block = binding("block-f17", "F17", Injected::Accept, EngineOp::Pause);
    block.action = Action::Block;
    block.widen = true;
    let mut e = engine_with(
        vec![
            remap_binding("f15-f16", "F15", "F16", None),
            block,
            remap_binding("f18-dual", "F18", "F19", Some("F20")),
        ],
        Duration::from_secs(10),
        false,
    );
    let s = Arc::clone(&e.stats);
    let physical = || s.physical.load(Ordering::Relaxed);
    let own = || s.own_injected.load(Ordering::Relaxed);
    let mut verified: Vec<&str> = Vec::new();
    // Steps 1–5 use remaps and a block, which match with any modifiers held (widening) and do not
    // depend on other keys: physical input cannot change them, so they are plain assertions.
    // 1. F15 acts as F16 for exactly as long as it is held; F15 itself never goes down.
    let t = Instant::now();
    inject_or_skip!(send_other(VK_F15, false), "F15 down", verified);
    let pressed = eventually(1000, || key_down(VK_F16));
    let latency = t.elapsed();
    if !pressed && !ddalgak::win::input_desktop_available() {
        eprintln!("skipped (locked during the run): verified before: {verified:?}");
        return;
    }
    assert!(pressed, "F16 is held while F15 is");
    assert!(!key_down(VK_F15), "the trigger was consumed");
    eprintln!("remap output arrived {latency:?} after the trigger");
    inject_or_skip!(send_other(VK_F15, false), "F15 repeat", verified);
    std::thread::sleep(Duration::from_millis(50));
    assert!(key_down(VK_F16), "a repeat keeps the target down");
    inject_or_skip!(send_other(VK_F15, true), "F15 up", verified);
    assert!(
        eventually(1000, || !key_down(VK_F16)),
        "F16 released with F15"
    );
    verified.push("remap F15 -> F16: target held with the trigger, released with it");
    // 2. F17 is blocked: consumed down and up, never logically pressed.
    let consumed0 = s.consumed.load(Ordering::Relaxed);
    inject_or_skip!(send_other(VK_F17, false), "F17 down", verified);
    std::thread::sleep(Duration::from_millis(30));
    assert!(!key_down(VK_F17), "a blocked key never goes down");
    inject_or_skip!(send_other(VK_F17, true), "F17 up", verified);
    assert!(eventually(500, || s.consumed.load(Ordering::Relaxed) >= consumed0 + 2));
    verified.push("block F17: down and up consumed");
    // 3. The output is Ddalgak's own input: the hook passed F16 on without matching it.
    assert!(own() >= 3, "F16 down, repeat, up seen as own");
    // 4. Pausing (the core cancels and tells the worker to release everything) releases a held
    //    target at once; the trigger's later up stays consumed and presses nothing again.
    inject_or_skip!(send_other(VK_F15, false), "F15 down again", verified);
    assert!(eventually(1000, || key_down(VK_F16)));
    e.hook.as_ref().unwrap().cancel_pending();
    assert!(
        eventually(1000, || !key_down(VK_F16)),
        "released by the pause path"
    );
    inject_or_skip!(send_other(VK_F15, true), "F15 up after the pause", verified);
    std::thread::sleep(Duration::from_millis(50));
    assert!(!key_down(VK_F16) && !key_down(VK_F15));
    verified.push("pause path releases a held target");
    // 5. A desktop switch, an unlock or an elevated window (key-state reset) releases too.
    inject_or_skip!(send_other(VK_F15, false), "F15 down for a reset", verified);
    assert!(eventually(1000, || key_down(VK_F16)));
    e.shared.request_key_reset();
    assert!(
        eventually(1000, || !key_down(VK_F16)),
        "released by the reset path"
    );
    inject_or_skip!(send_other(VK_F15, true), "F15 up after the reset", verified);
    verified.push("key-state reset releases a held target");
    // 6. Dual role F18: a quick tap sends F20 (seen as two own events), a hold past 200 ms
    //    holds F19 (the hook thread's one-shot timer), released with F18. By design any other
    //    key that goes down while the dual role is undecided decides it at once (the target
    //    first, DDALGAK_RULE_MODEL §5.1): the person typing on this desktop, or another program's
    //    injected key. So both run as attempts judged with the hook's own counters.
    //
    //    Whatever decided it, nothing may stay pressed: that is asserted in every attempt. The
    //    key states are awaited, because the hook counts F20's up before Windows updates the
    //    async key state (the Step 74 merge-gate failure read F20 once, right after the count).
    let all_up = || !key_down(VK_F20) && !key_down(VK_F19) && !key_down(VK_F18);
    step!(
        quiet::attempts(
            "dual role F18: a tap sends F20",
            physical,
            |_| {
                let own0 = own();
                sent(send_other(VK_F18, false), "F18 down (tap)")?;
                sent(send_other(VK_F18, true), "F18 up (tap)")?;
                let both = eventually(1000, || own() >= own0 + 2);
                assert!(
                    eventually(1000, all_up),
                    "nothing is left pressed after a tap"
                );
                check(both, || {
                    format!(
                        "the tap output went down and up: {} own events",
                        own() - own0
                    )
                })
            },
            || {},
        ),
        "dual role F18: a tap sends F20",
        verified
    );
    // The hold. In a clean attempt this hook saw exactly two key events (F18 down, then its own
    // F19 down) and its thread woke for the one-shot timer; only such an attempt is held to the
    // tap time.
    let held = step!(
        quiet::attempts(
            "dual role F18: a hold presses F19 after the tap time",
            physical,
            |_| {
                let events0 = s.callbacks.load(Ordering::Relaxed);
                let other0 = s.other_injected.load(Ordering::Relaxed);
                let wakeups0 = s.hook_wakeups.load(Ordering::Relaxed);
                let t = Instant::now();
                sent(send_other(VK_F18, false), "F18 down (hold)")?;
                let pressed = eventually(1500, || key_down(VK_F19));
                let waited = t.elapsed();
                let events = s.callbacks.load(Ordering::Relaxed) - events0;
                let others = s.other_injected.load(Ordering::Relaxed) - other0;
                let timer = s.hook_wakeups.load(Ordering::Relaxed) - wakeups0;
                sent(send_other(VK_F18, true), "F18 up (hold)")?;
                assert!(eventually(1000, all_up), "F19 released with F18");
                check(pressed, || "the dual role pressed F19".into())?;
                if others > 1 {
                    return Err(Fail::Disturbed(format!(
                        "another program injected {} key events during the tap time",
                        others - 1
                    )));
                }
                check(events <= 2, || {
                    format!(
                        "{events} key events reached the hook instead of 2 (decided after \
                         {waited:?})"
                    )
                })?;
                // Undisturbed, only the hook thread's timer decides the hold: never before the
                // tap time (200 ms on GetTickCount, which has ~16 ms steps; load only adds).
                check(waited >= Duration::from_millis(150), || {
                    format!("not before the tap time: {waited:?}")
                })?;
                check(timer >= 1, || {
                    "the hook thread's one-shot timer decided the hold".into()
                })?;
                Ok(waited)
            },
            || {},
        ),
        "dual role F18: a hold presses F19 after the tap time",
        verified
    );
    if let Some(waited) = held {
        eprintln!("dual role: F19 pressed {waited:?} after F18 went down (tap time 200 ms)");
    }
    let p99 = s.p99_us().unwrap_or(0);
    eprintln!(
        "remap smoke: callbacks {} ({} physical), consumed {}, own events {}, max {} µs, \
         p99 ≤ {} µs",
        s.callbacks.load(Ordering::Relaxed),
        s.physical.load(Ordering::Relaxed),
        s.consumed.load(Ordering::Relaxed),
        own(),
        s.max_us.load(Ordering::Relaxed),
        p99
    );
    e.hook.take().unwrap().stop();
    drop(e.worker);
    for vk in [VK_F15, VK_F16, VK_F17, VK_F18, VK_F19, VK_F20] {
        assert!(
            eventually(1000, || !key_down(vk)),
            "nothing is left pressed ({vk:#x})"
        );
    }
    eprintln!("verified with real SendInput: {verified:?}");
}

/// Step 69: the typing-run path is wired to the real `SendInput`. A `send_keys` run, fired through
/// the worker queue exactly as the core fires it, taps `F16` (an obscure key; never a character),
/// and the observe-only hook sees both events as Ddalgak's own input. The run's target checks use
/// a fixed editable target here: the real probe would look at the maintainer's focused window.
#[test]
fn a_send_keys_run_reaches_send_input_with_an_obscure_key_only() {
    use ddalgak::core::{FireKind, Signal};
    let _turn = serial();
    let mut b = binding("f13-sends-f16", "F13", Injected::Ignore, EngineOp::Pause);
    b.injects = true;
    b.action = Action::Keys(Arc::new(ddalgak::run::KeysSpec {
        chords: vec![Output::parse("F16").unwrap()],
        step_delay_ms: 0,
    }));
    let e = engine(vec![b], Duration::from_secs(10));
    if !ddalgak::win::input_desktop_available() {
        eprintln!("skipped (locked): SendInput is refused without an interactive desktop");
        return;
    }
    let before = e.stats.own_injected.load(Ordering::Relaxed);
    e.worker.queue().signal(
        1,
        Signal::Fire {
            binding: 0,
            kind: FireKind::Press,
        },
    );
    let t = Instant::now();
    let outcome = loop {
        let last = e
            .worker
            .activity
            .lock()
            .unwrap()
            .entries()
            .last()
            .map(|x| (x.rule.clone(), x.outcome.clone()));
        if let Some(o) = last {
            break o;
        }
        assert!(t.elapsed() < Duration::from_secs(5), "the run never ended");
        std::thread::sleep(Duration::from_millis(5));
    };
    if outcome.1 == "injection_failed" {
        eprintln!("skipped (SendInput refused, e.g. an elevated window has focus)");
        return;
    }
    assert_eq!(outcome, ("f13-sends-f16".to_string(), "ok".to_string()));
    while e.stats.own_injected.load(Ordering::Relaxed) < before + 2 {
        assert!(
            t.elapsed() < Duration::from_secs(5),
            "the hook never saw the run's F16"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        eventually(1000, || !key_down(VK_F16)),
        "F16 is not left pressed"
    );
    eprintln!(
        "send_keys run: F16 tapped through SendInput, seen by the hook as own input in {:?}",
        t.elapsed()
    );
}

/// Step 71: the trigger path of mouse actions with the real hook and real injected obscure keys
/// (F13–F15 as "another program"; these bindings accept injected input, which a rules file never
/// allows for injecting rules, to stand in for physical keys). The mouse output goes to a
/// **simulated pointer** only: the worker's backend and screen are a `SimPointer`, so the real
/// pointer is never moved, scrolled or clicked (the maintainer uses it). The hook consumes only
/// F13–F15 and passes every other event on.
#[test]
fn mouse_rules_on_obscure_keys_reach_only_a_simulated_pointer() {
    use ddalgak::mouse::{
        Layout, MotionKind, MotionSpec, MousePlan, MouseStep, Point, ScrollDir, SimPointer, Unit,
    };
    let _turn = serial();
    if !ddalgak::win::input_desktop_available() {
        eprintln!(
            "skipped (locked): no interactive input desktop; mouse actions are covered by the mouse, core and worker unit tests"
        );
        return;
    }
    let mouse = |rule: &str, keys: &str, action: Action| {
        let mut b = binding(rule, keys, Injected::Accept, EngineOp::Pause);
        b.injects = true;
        b.action = action;
        b
    };
    let bindings = vec![
        mouse(
            "f13-drags",
            "F13",
            Action::MouseHold {
                spec: RemapSpec {
                    to: Output::button(ddalgak::inject::Button::Left),
                    tap: None,
                    tap_ms: 200,
                },
                at: None,
            },
        ),
        mouse(
            "f14-glides",
            "F14",
            Action::Motion(MotionSpec {
                dx: 1,
                dy: 0,
                unit: Unit::Pixels,
                kind: MotionKind::Continuous {
                    speed: 2000,
                    acceleration: 0,
                },
            }),
        ),
        mouse(
            "f15-scrolls",
            "F15",
            Action::Mouse(Arc::new(MousePlan {
                steps: vec![MouseStep::Scroll {
                    dir: ScrollDir::Down,
                    notches: 2,
                }],
                stop_on_error: true,
            })),
        ),
    ];
    let sim = SimPointer::new(Layout::single(1920, 1080), Point::new(500, 500));
    let stats = Arc::new(Stats::default());
    let shared = Shared::new(Arc::clone(&stats));
    let worker = Worker::start(
        Arc::clone(&stats),
        Box::new(|_| {}),
        Arc::new(Mutex::new(Activity::default())),
        Box::new(sim.backend()),
        ddalgak::run::Env {
            screen: Box::new(sim.screen()),
            ..ddalgak::run::Env::inert()
        },
    );
    worker.queue().send(Msg::Rules(Rules {
        generation: 1,
        bindings: Arc::new(bindings.clone()),
        profile: "default".into(),
        mode: "base".into(),
        modes: ddalgak::modes::ModeSet::single(),
    }));
    shared.set_state(STATE_RUNNING);
    let hook = HookThread::start(
        Table::compile(bindings, 20),
        1,
        &shared,
        worker.queue(),
        hook::Options {
            observe_only: false,
            max_lifetime: Some(Duration::from_secs(10)),
        },
    )
    .expect("Windows installs a low-level keyboard hook for a normal user process");
    let mut verified: Vec<&str> = Vec::new();
    // Mouse rules match their triggers exactly: a modifier a person holds keeps them from
    // matching, so each step runs as attempts judged with the hook's count of physical events.
    let physical = || stats.physical.load(Ordering::Relaxed);
    // 1. F15: two wheel notches down on the simulated pointer.
    let t = Instant::now();
    let scrolled = quiet::attempts(
        "a scroll rule fires on a real key event",
        physical,
        |_| {
            let (w, h) = sim.with(|s| s.wheel);
            sent(tap_other(VK_F15) == 2, "F15 tap")?;
            let ok = eventually(2000, || sim.with(|s| s.wheel) == (w - 2, h));
            if !ok && !ddalgak::win::input_desktop_available() {
                return Err(Fail::Refused("F15 tap (locked during the run)".into()));
            }
            check(ok, || format!("the wheel rule ran: {:?}", sim.events()))
        },
        || {},
    );
    if step!(
        scrolled,
        "a scroll rule fires on a real key event and scrolls the simulated pointer",
        verified
    )
    .is_some()
    {
        eprintln!("F15 -> two simulated wheel notches in {:?}", t.elapsed());
    }
    // 2. F13 held: the left button is down exactly as long.
    step!(
        quiet::attempts(
            "a held button follows its real trigger key",
            physical,
            |_| {
                sent(send_other(VK_F13, false), "F13 down")?;
                let down = eventually(2000, || sim.with(|s| s.buttons.len()) == 1);
                sent(send_other(VK_F13, true), "F13 up")?;
                assert!(
                    eventually(2000, || sim.with(|s| s.buttons.is_empty())),
                    "the button is never left down"
                );
                check(down, || "the left button went down with F13".into())
            },
            || {},
        ),
        "a held button goes down and up with its real trigger key",
        verified
    );
    // 3. F14 held: the pointer glides right; F14 up stops it.
    step!(
        quiet::attempts(
            "a move while held glides with its real trigger key",
            physical,
            |_| {
                let x0 = sim.cursor().x;
                sent(send_other(VK_F14, false), "F14 down")?;
                let glided = eventually(2000, || sim.cursor().x >= x0 + 100);
                sent(send_other(VK_F14, true), "F14 up")?;
                std::thread::sleep(Duration::from_millis(50));
                let stopped = sim.cursor();
                std::thread::sleep(Duration::from_millis(100));
                assert_eq!(sim.cursor(), stopped, "the motion stopped with the trigger");
                check(glided, || format!("the pointer glided right from {x0}"))
            },
            || {},
        ),
        "a move while held glides until its real trigger key goes up",
        verified
    );
    // The triggers were consumed: none of them is left pressed for applications.
    for vk in [VK_F13, VK_F14, VK_F15] {
        assert!(eventually(1000, || !key_down(vk)), "{vk:#x}");
    }
    drop(hook);
    drop(worker);
    assert!(sim.with(|s| s.buttons.is_empty()));
    eprintln!("verified with real hook events: {verified:?}");
}

/// Step 75: the attempt harness itself, with a fake physical-event counter and modifier probe (no
/// input is injected): a disturbed failure is retried, an undisturbed failure fails the test, a
/// refusal ends the step, and a step disturbed in every attempt is never reported as verified.
#[test]
fn the_attempt_harness_retries_only_disturbed_failures() {
    use std::cell::Cell;
    let physical = Cell::new(0u64);
    let none = Vec::<&'static str>::new;
    // Attempt 1 fails while a person typed (the counter moved); attempt 2 is clean and passes.
    let resets = Cell::new(0);
    let r = quiet::attempts_with(
        "disturbed once",
        || physical.get(),
        none,
        |n| {
            if n == 1 {
                physical.set(physical.get() + 3);
                return Err(Fail::Failed("the rule did not fire".into()));
            }
            Ok(n)
        },
        || resets.set(resets.get() + 1),
    );
    assert!(matches!(r, Outcome::Verified(2)), "{r:?}");
    assert_eq!(resets.get(), 1, "cleaned up once between the attempts");
    // A modifier held during an attempt also makes its failure a disturbed one.
    let shift = Cell::new(false);
    let r = quiet::attempts_with(
        "a held modifier",
        || physical.get(),
        || {
            if shift.get() { vec!["LShift"] } else { vec![] }
        },
        |n| {
            shift.set(n == 1);
            check(n == 2, || "no match while Shift was held".into()).map(|()| n)
        },
        || shift.set(false),
    );
    assert!(matches!(r, Outcome::Verified(2)), "{r:?}");
    // An attempt that saw another program's input says so itself.
    let r = quiet::attempts_with(
        "foreign injected input",
        || physical.get(),
        none,
        |n| {
            if n < 3 {
                Err(Fail::Disturbed("another program injected a key".into()))
            } else {
                Ok(())
            }
        },
        || {},
    );
    assert!(matches!(r, Outcome::Verified(())), "{r:?}");
    // Disturbed in every attempt: skipped, never verified.
    let r: Outcome<()> = quiet::attempts_with(
        "always disturbed",
        || {
            physical.set(physical.get() + 1);
            physical.get()
        },
        none,
        |_| Err(Fail::Failed("never fires".into())),
        || {},
    );
    assert!(matches!(r, Outcome::Disturbed), "{r:?}");
    // A refused SendInput ends the step at once.
    let tries = Cell::new(0);
    let r: Outcome<()> = quiet::attempts_with(
        "refused",
        || physical.get(),
        none,
        |_| {
            tries.set(tries.get() + 1);
            sent(false, "F13 down")
        },
        || {},
    );
    assert!(
        matches!(r, Outcome::Refused(ref s) if s == "F13 down"),
        "{r:?}"
    );
    assert_eq!(tries.get(), 1);
    // A failure without any physical input is a real failure: it is never retried away.
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        quiet::attempts_with(
            "a real bug",
            || physical.get(),
            none,
            |_| -> quiet::Step<()> { Err(Fail::Failed("the target stayed up".into())) },
            || {},
        )
    }));
    let message = caught.expect_err("an undisturbed failure panics");
    let text = message
        .downcast_ref::<String>()
        .cloned()
        .unwrap_or_default();
    assert!(
        text.contains("a real bug") && text.contains("no physical input"),
        "{text}"
    );
}
