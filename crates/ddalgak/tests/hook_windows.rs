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
//! Requirements: `DDALGAK-NFR-001`, `DDALGAK-NFR-002`, `DDALGAK-SEC-004`, `DDALGAK-SEC-005`,
//! `DDALGAK-FR-002`.
#![cfg(windows)]

use ddalgak::inject::{Injector, OutKey};
use ddalgak::stats::Stats;
use ddalgak::table::{Action, Binding, EngineOp, Injected, On, Table};
use ddalgak::trigger::Trigger;
use ddalgak::win::hook::{self, HookThread, STATE_RUNNING, Shared};
use ddalgak::win::inject::{SendInputBackend, send_as_other_program};
use ddalgak::worker::{Activity, Control, Msg, Rules, Worker};
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
    );
    worker.queue().send(Msg::Rules(Rules {
        generation: 1,
        bindings: Arc::new(bindings.clone()),
        profile: "default".into(),
        mode: "base".into(),
    }));
    shared.set_state(STATE_RUNNING);
    let t = Instant::now();
    let hook = HookThread::start(
        Table::compile(bindings, 20),
        1,
        &shared,
        worker.queue(),
        hook::Options {
            observe_only: true,
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

#[test]
fn real_hook_idle_cost_injected_cycles_held_keys_and_cleanup() {
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

fn injected(e: &mut Engine) {
    let s = Arc::clone(&e.stats);
    let mut verified: Vec<&str> = Vec::new();
    let own0 = s.own_injected.load(Ordering::Relaxed);
    let other0 = s.other_injected.load(Ordering::Relaxed);
    // 1. Ddalgak's own input (marked) is passed on and never matched, even by an accepting rule:
    //    an injected cycle cannot re-trigger the engine.
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
    // 2. Another program's input reaches only `injected: accept` rules.
    inject_or_skip!(tap_other(VK_F14) == 2, "other-program F14", verified);
    inject_or_skip!(tap_other(VK_F13) == 2, "other-program F13", verified);
    let c = e
        .control
        .recv_timeout(Duration::from_secs(3))
        .expect("accept-f13 fired");
    assert_eq!(
        c,
        Control::Engine {
            op: EngineOp::Pause,
            rule: "accept-f13".into()
        }
    );
    std::thread::sleep(Duration::from_millis(200));
    assert!(e.control.try_recv().is_err(), "ignore-f14 did not fire");
    let entries: Vec<(String, String)> = e
        .worker
        .activity
        .lock()
        .unwrap()
        .entries()
        .map(|x| (x.rule.clone(), x.outcome.clone()))
        .collect();
    assert!(
        entries.contains(&("ignore-f14".into(), "skipped_injected_input".into())),
        "{entries:?}"
    );
    assert!(
        entries.contains(&("accept-f13".into(), "ok".into())),
        "{entries:?}"
    );
    assert!(s.other_injected.load(Ordering::Relaxed) - other0 >= 4);
    assert_eq!(
        s.consumed.load(Ordering::Relaxed),
        0,
        "observe-only: nothing consumed"
    );
    verified.push("other programs' input reaches accept rules only");
    // 3. Held keys are released on release_all (pause/stop/emergency paths) and on drop.
    inject_or_skip!(inj.press(VK_F16), "press F16", verified);
    assert!(key_down(VK_F16), "F16 is logically down");
    assert_eq!(inj.release_all(), 1);
    assert!(!key_down(VK_F16), "release_all released F16");
    {
        let mut scoped = Injector::new(SendInputBackend);
        inject_or_skip!(scoped.press(VK_F17), "press F17", verified);
        assert!(key_down(VK_F17));
    }
    assert!(!key_down(VK_F17), "Drop released F17");
    verified.push("held keys are released by release_all and on drop");
    // 4. A loop through another program is cut by the rate limit (20 per second).
    while e.control.try_recv().is_ok() {}
    let count_limited = |e: &Engine| {
        e.worker
            .activity
            .lock()
            .unwrap()
            .entries()
            .filter(|x| x.outcome == "rate_limited")
            .count()
    };
    let limited0 = count_limited(e);
    for _ in 0..40 {
        inject_or_skip!(tap_other(VK_F13) == 2, "F13 storm", verified);
    }
    std::thread::sleep(Duration::from_millis(500));
    let fired = std::iter::from_fn(|| e.control.try_recv().ok()).count();
    assert!(fired <= 20, "at most max_runs_per_second runs: {fired}");
    let limited = count_limited(e) - limited0;
    assert!(limited >= 19, "rate_limited outcomes recorded: {limited}");
    verified.push("a 40-event storm through another program is rate-limited");
    // 5. Callback latency (DDALGAK-NFR-002: p99 < 1 ms).
    let p99 = s.p99_us().unwrap();
    eprintln!(
        "hook callbacks: {} total, max {} µs, p99 ≤ {} µs, max event delay {} ms",
        s.callbacks.load(Ordering::Relaxed),
        s.max_us.load(Ordering::Relaxed),
        p99,
        s.max_delay_ms.load(Ordering::Relaxed)
    );
    assert!(p99 <= 1_000, "p99 callback time {p99} µs");
    verified.push("p99 callback time below 1 ms");
    eprintln!("verified with real SendInput: {verified:?}");
}
