//! The worker thread: everything that happens after the hook decided (ADR 0024 §3).
//!
//! The hook callback only pushes a small [`Signal`] into a bounded queue (`try_send`; a full
//! queue drops the signal and counts it, it never blocks the callback). The worker runs the
//! fired rules' steps, times `hold` triggers, and records outcomes in the [`Activity`] list.
//! It sleeps in a blocking receive; a timeout exists only while a hold trigger is down, so an
//! idle engine has no timer (`DDALGAK-NFR-001`).
//!
//! Engine-control steps are handed to the controller (the host's main thread), which owns the
//! lifecycle; the worker never changes the engine state itself.
//!
//! **Output** (Step 68): the worker owns the [`Injector`] and its ledger. It carries out the
//! core's remap instructions ([`RemapOp`]) exactly as given, sends on keys the core held back
//! ([`Signal::Replay`]) and releases everything it holds on [`Msg::Cancel`] (pause, stop, session
//! lock, profile switch), on [`Signal::ReleaseAll`] (sent by the hook thread after it dropped its
//! remaps, so it is ordered after every press the hook decided before), on the emergency chord and
//! when it stops (`Drop`). Remap instructions carry their output keys, so a held output is released
//! correctly even after a reload changed the rules; only the activity entry needs the current
//! rules. A repeat is sent only for an output the ledger still holds, so a late repeat can never
//! press again what a release-all let go.
//!
//! **Typing runs** (Step 69, [`crate::run`]): `type_text` and `send_keys` rules and hotstrings run
//! one at a time as a plan of pieces, carried out between the worker's other messages (one
//! `SendInput` call per piece, waits as timeouts), so remaps keep working while text is typed.
//! A trigger while a run is in progress is dropped with `busy` (a hotstring's consumed key is
//! given back). Pause, stop, session lock, a reload, the emergency chord and the worker's end
//! cancel the run at its next piece (`cancelled`), after restoring the clipboard and the
//! modifiers. A run has a timeout only while it waits, so an idle engine still has no timer.

use crate::core::{RemapOp, Signal};
use crate::inject::{Backend, Injector, OutCode, OutEvent};
use crate::run::{self, Env, HotstringFire, Next, Run, RunShared};
use crate::stats::Stats;
use crate::table::{Action, Binding, EngineOp};
use crate::vk::ModKey;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Capacity of the hook → worker queue. A burst larger than this is an event storm.
pub const QUEUE: usize = 1024;

/// Most activity entries kept in memory (DDALGAK_RULE_MODEL §12).
pub const ACTIVITY_MAX: usize = 200;

/// One activity entry: rule ID and outcome code only (`DDALGAK-SEC-001`, `DDALGAK-SEC-011`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Entry {
    pub at: String,
    pub rule: String,
    pub profile: String,
    pub mode: String,
    pub outcome: String,
}

/// The recent outcomes, newest last.
#[derive(Clone, Debug, Default)]
pub struct Activity {
    entries: VecDeque<Entry>,
}

impl Activity {
    pub fn push(&mut self, e: Entry) {
        if self.entries.len() == ACTIVITY_MAX {
            self.entries.pop_front();
        }
        self.entries.push_back(e);
    }

    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The last `n` entries as JSON.
    pub fn to_json(&self, n: usize) -> Value {
        let skip = self.entries.len().saturating_sub(n);
        Value::Array(
            self.entries
                .iter()
                .skip(skip)
                .map(|e| {
                    json!({ "at": e.at, "rule": e.rule, "profile": e.profile, "mode": e.mode,
                            "outcome": e.outcome })
                })
                .collect(),
        )
    }
}

/// What the worker asks the controller to do.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Control {
    /// An engine-control step of a rule.
    Engine { op: EngineOp, rule: String },
    /// The emergency chord fired.
    Emergency,
}

/// The rules the worker runs, with the generation the hook's table has.
#[derive(Clone, Debug)]
pub struct Rules {
    pub generation: u32,
    pub bindings: Arc<Vec<Binding>>,
    pub profile: String,
    pub mode: String,
}

/// Messages to the worker.
#[derive(Debug)]
pub enum Msg {
    Signal {
        generation: u32,
        signal: Signal,
    },
    Rules(Rules),
    /// Cancel timers (pause, stop, profile switch).
    Cancel,
    Stop,
}

/// The sending end the hook uses: never blocks.
#[derive(Clone, Debug)]
pub struct Queue {
    tx: SyncSender<Msg>,
    stats: Arc<Stats>,
    runs: Arc<RunShared>,
}

impl Queue {
    /// Pushes a signal from the hook callback. A full queue drops it (counted).
    pub fn signal(&self, generation: u32, signal: Signal) {
        match self.tx.try_send(Msg::Signal { generation, signal }) {
            Ok(()) => {}
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.stats.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Control messages from the host (may wait briefly when the queue is full).
    pub fn send(&self, msg: Msg) {
        let _ = self.tx.send(msg);
    }

    /// What the hook's core shares with the typing runs (modifier state, key count).
    pub fn runs(&self) -> Arc<RunShared> {
        Arc::clone(&self.runs)
    }
}

/// The running worker; stops and joins on drop.
#[derive(Debug)]
pub struct Worker {
    queue: Queue,
    join: Option<std::thread::JoinHandle<()>>,
    pub activity: Arc<Mutex<Activity>>,
}

type Sink = Box<dyn Fn(Control) + Send>;

impl Worker {
    /// Starts the worker. `control` receives engine-control requests (and must wake the
    /// controller); `activity` is shared with status reporting; `backend` injects the output
    /// (`SendInput` in the app, a recorder in tests); `env` is what typing runs ask the platform
    /// (the focused target, the clipboard, the local time).
    pub fn start(
        stats: Arc<Stats>,
        control: Sink,
        activity: Arc<Mutex<Activity>>,
        backend: Box<dyn Backend>,
        env: Env,
    ) -> Worker {
        let (tx, rx) = sync_channel::<Msg>(QUEUE);
        let act = Arc::clone(&activity);
        let st = Arc::clone(&stats);
        let runs = Arc::new(RunShared::default());
        let shared = Arc::clone(&runs);
        let join = std::thread::Builder::new()
            .name("ddalgak-worker".into())
            .spawn(move || {
                let mut inj = Injector::new(backend);
                let mut env = env;
                let mut ctx = Ctx {
                    inj: &mut inj,
                    env: &mut env,
                    shared: &shared,
                    activity: &act,
                    active: None,
                };
                run(&rx, &control, &st, &mut ctx);
            })
            .expect("spawn the worker thread");
        Worker {
            queue: Queue { tx, stats, runs },
            join: Some(join),
            activity,
        }
    }

    pub fn queue(&self) -> Queue {
        self.queue.clone()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.queue.send(Msg::Stop);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

struct Hold {
    generation: u32,
    binding: u16,
    due: Instant,
}

type Inj = Injector<Box<dyn Backend>>;

/// The typing run in progress and when its next piece is due.
struct Active {
    run: Run,
    due: Instant,
}

/// What the loop's typing runs work with.
struct Ctx<'a> {
    inj: &'a mut Inj,
    env: &'a mut Env,
    shared: &'a RunShared,
    activity: &'a Mutex<Activity>,
    active: Option<Active>,
}

impl Ctx<'_> {
    fn push(&self, run: &Run, outcome: &str) {
        if let Ok(mut a) = self.activity.lock() {
            a.push(Entry {
                at: crate::time::now_rfc3339(),
                rule: run.rule.clone(),
                profile: run.profile.clone(),
                mode: run.mode.clone(),
                outcome: outcome.to_string(),
            });
        }
    }

    /// Starts a typing run for a fired rule, or drops the trigger with `busy`.
    fn start(&mut self, r: &Rules, b: &Binding, hs: Option<HotstringFire>) {
        if self.active.is_some() {
            if let Ok(mut a) = self.activity.lock() {
                a.push(Entry {
                    at: crate::time::now_rfc3339(),
                    rule: b.rule.clone(),
                    profile: r.profile.clone(),
                    mode: r.mode.clone(),
                    outcome: "busy".into(),
                });
            }
            if let Some(h) = &hs {
                run::give_back(self.inj, h);
            }
            return;
        }
        let local = (self.env.clock)();
        let pieces = match &b.action {
            Action::Type(t) => run::plan_text(t, hs.as_ref(), &local),
            Action::Keys(k) => run::plan_keys(k, hs.as_ref()),
            _ => return,
        };
        let now = Instant::now();
        let run = Run::new(
            b.rule.clone(),
            r.profile.clone(),
            r.mode.clone(),
            pieces,
            hs,
            now,
        );
        self.active = Some(Active { run, due: now });
        self.step();
    }

    /// Carries out the run's next pieces when they are due.
    fn step(&mut self) {
        let Some(a) = self.active.as_mut() else {
            return;
        };
        let now = Instant::now();
        if a.due > now {
            return;
        }
        match a.run.step(now, self.inj, self.env, self.shared) {
            Next::At(t) => a.due = t,
            Next::Done(code) => {
                if let Some(a) = self.active.take() {
                    self.push(&a.run, code);
                }
            }
        }
    }

    /// Ends the run in progress (pause, stop, emergency, reload).
    fn cancel(&mut self) {
        if let Some(mut a) = self.active.take() {
            let code = a.run.cancel(self.inj, self.env, self.shared);
            self.push(&a.run, code);
        }
    }
}

fn mod_events(bits: u8, up: bool, tracked: bool, out: &mut Vec<(OutEvent, bool)>) {
    let mut keys: Vec<ModKey> = ModKey::ALL
        .into_iter()
        .filter(|m| bits & m.bit() != 0)
        .collect();
    if up {
        keys.reverse();
    }
    for m in keys {
        out.push((
            OutEvent {
                code: OutCode::Vk(m.vk()),
                up,
            },
            tracked,
        ));
    }
}

/// Carries out one remap instruction. Returns whether every event was inserted.
pub fn execute(inj: &mut Injector<impl Backend>, op: RemapOp) -> bool {
    let mut ev: Vec<(OutEvent, bool)> = Vec::new();
    match op {
        RemapOp::Press { lift, mods, key } => {
            mod_events(lift, true, false, &mut ev);
            mod_events(mods, false, true, &mut ev);
            if let Some(k) = key {
                ev.push((OutEvent::down(k), true));
            }
        }
        RemapOp::Repeat { key } => {
            if !inj.is_held(key) {
                return true;
            }
            ev.push((OutEvent::down(key), true));
        }
        RemapOp::Release { key, mods, restore } => {
            if let Some(k) = key.filter(|k| inj.is_held(*k)) {
                ev.push((OutEvent::up(k), true));
            }
            let held: u8 = ModKey::ALL
                .into_iter()
                .filter(|m| mods & m.bit() != 0 && inj.is_held(OutCode::Vk(m.vk())))
                .fold(0, |a, m| a | m.bit());
            mod_events(held, true, true, &mut ev);
            mod_events(restore, false, false, &mut ev);
        }
        RemapOp::Tap {
            lift,
            mods,
            key,
            restore,
        } => {
            mod_events(lift, true, false, &mut ev);
            mod_events(mods, false, true, &mut ev);
            if let Some(k) = key {
                ev.push((OutEvent::down(k), true));
                ev.push((OutEvent::up(k), true));
            }
            mod_events(mods, true, true, &mut ev);
            mod_events(restore, false, false, &mut ev);
        }
    }
    inj.send_mixed(&ev)
}

#[allow(clippy::too_many_lines)]
fn run(rx: &Receiver<Msg>, control: &Sink, stats: &Stats, ctx: &mut Ctx<'_>) {
    let mut rules: Option<Rules> = None;
    let mut holds: Vec<Hold> = Vec::new();
    let activity = ctx.activity;
    loop {
        let next = holds
            .iter()
            .map(|h| h.due)
            .chain(ctx.active.as_ref().map(|a| a.due))
            .min();
        let msg = match next {
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            Some(due) => rx.recv_timeout(due.saturating_duration_since(Instant::now())),
        };
        stats.worker_wakeups.fetch_add(1, Ordering::Relaxed);
        let inj = &mut *ctx.inj;
        match msg {
            Err(RecvTimeoutError::Disconnected) | Ok(Msg::Stop) => {
                ctx.cancel();
                break;
            }
            Err(RecvTimeoutError::Timeout) => {
                let now = Instant::now();
                let due: Vec<Hold> = {
                    let (due, keep): (Vec<Hold>, Vec<Hold>) =
                        holds.drain(..).partition(|h| h.due <= now);
                    holds = keep;
                    due
                };
                for h in due {
                    if fire(rules.as_ref(), h.generation, h.binding, control, activity)
                        && let Some((r, b)) = current(rules.as_ref(), h.generation, h.binding)
                    {
                        ctx.start(r, b, None);
                    }
                }
            }
            Ok(Msg::Rules(r)) => {
                holds.clear();
                ctx.cancel();
                rules = Some(r);
            }
            Ok(Msg::Cancel) => {
                holds.clear();
                ctx.cancel();
                ctx.inj.release_all();
            }
            Ok(Msg::Signal { generation, signal }) => match signal {
                Signal::Emergency => {
                    holds.clear();
                    ctx.cancel();
                    ctx.inj.release_all();
                    control(Control::Emergency);
                }
                Signal::Hotstring { binding, fire } => {
                    match current(rules.as_ref(), generation, binding) {
                        Some((r, b)) if b.action.is_run() => ctx.start(r, b, Some(fire)),
                        // A reload came in between: nothing runs, the key goes back.
                        _ => run::give_back(inj, &fire),
                    }
                }
                Signal::Remap { binding, op } => {
                    let ok = execute(inj, op);
                    if matches!(op, RemapOp::Press { .. } | RemapOp::Tap { .. }) {
                        let outcome = if ok { "ok" } else { "injection_failed" };
                        record(rules.as_ref(), generation, binding, outcome, activity);
                    }
                }
                Signal::Replay { code, up } => {
                    if up {
                        inj.release_code(code);
                    } else {
                        inj.press_code(code);
                    }
                }
                Signal::ReleaseAll => {
                    inj.release_all();
                }
                Signal::Restore { mods } => {
                    let mut ev = Vec::new();
                    mod_events(mods, false, false, &mut ev);
                    inj.send_mixed(&ev);
                }
                Signal::Fire { binding, .. } => {
                    if fire(rules.as_ref(), generation, binding, control, activity)
                        && let Some((r, b)) = current(rules.as_ref(), generation, binding)
                    {
                        ctx.start(r, b, None);
                    }
                }
                Signal::Skipped { binding, code } => {
                    record(rules.as_ref(), generation, binding, code.as_str(), activity);
                }
                Signal::RateLimited { binding } => {
                    record(
                        rules.as_ref(),
                        generation,
                        binding,
                        "rate_limited",
                        activity,
                    );
                }
                Signal::HoldArmed { binding, hold_ms } => holds.push(Hold {
                    generation,
                    binding,
                    due: Instant::now() + Duration::from_millis(u64::from(hold_ms)),
                }),
                Signal::HoldCancelled { binding } => {
                    holds.retain(|h| !(h.binding == binding && h.generation == generation));
                }
            },
        }
        ctx.step();
    }
}

fn current(rules: Option<&Rules>, generation: u32, binding: u16) -> Option<(&Rules, &Binding)> {
    let r = rules.filter(|r| r.generation == generation)?;
    r.bindings.get(usize::from(binding)).map(|b| (r, b))
}

fn record(
    rules: Option<&Rules>,
    generation: u32,
    binding: u16,
    outcome: &str,
    activity: &Mutex<Activity>,
) {
    let Some((r, b)) = current(rules, generation, binding) else {
        return;
    };
    if let Ok(mut a) = activity.lock() {
        a.push(Entry {
            at: crate::time::now_rfc3339(),
            rule: b.rule.clone(),
            profile: r.profile.clone(),
            mode: r.mode.clone(),
            outcome: outcome.to_string(),
        });
    }
}

/// Runs a fired rule. Signals of an older table generation are dropped (a reload happened).
/// Returns `true` for a typing rule, which the caller starts as a run.
fn fire(
    rules: Option<&Rules>,
    generation: u32,
    binding: u16,
    control: &Sink,
    activity: &Mutex<Activity>,
) -> bool {
    let Some((_, b)) = current(rules, generation, binding) else {
        return false;
    };
    match &b.action {
        Action::Engine(ops) => {
            for op in ops {
                control(Control::Engine {
                    op: *op,
                    rule: b.rule.clone(),
                });
            }
        }
        // The key was consumed; nothing else happens (§5.2).
        Action::Block => {}
        // Remaps run through `Signal::Remap`; a fired remap does nothing more.
        Action::Remap(_) => return false,
        // Typing runs record their own outcome when they end.
        Action::Type(_) | Action::Keys(_) => return true,
    }
    record(rules, generation, binding, "ok", activity);
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{FireKind, Skip};
    use crate::table::{Injected, On};
    use crate::trigger::Trigger;
    use std::sync::mpsc::channel;

    fn rules(generation: u32) -> Rules {
        let b = |id: &str, op| Binding {
            rule: id.into(),
            trigger: Trigger::parse("F13").unwrap(),
            on: On::Press,
            repeat: false,
            pass_through: false,
            injected: Injected::Ignore,
            widen: false,
            injects: false,
            action: Action::Engine(vec![op]),
            hotstring: None,
        };
        Rules {
            generation,
            bindings: Arc::new(vec![
                b("pause-it", EngineOp::Pause),
                b("off-it", EngineOp::Disable),
            ]),
            profile: "default".into(),
            mode: "base".into(),
        }
    }

    fn worker() -> (Worker, Receiver<Control>, Arc<Stats>) {
        let (tx, rx) = channel();
        let tx = Mutex::new(tx);
        let stats = Arc::new(Stats::default());
        let w = Worker::start(
            Arc::clone(&stats),
            Box::new(move |c| {
                let _ = tx.lock().unwrap().send(c);
            }),
            Arc::new(Mutex::new(Activity::default())),
            Box::new(Recorder::default()),
            Env::inert(),
        );
        (w, rx, stats)
    }

    #[test]
    fn fired_rules_reach_the_controller_and_the_activity_list() {
        let (w, rx, _) = worker();
        let q = w.queue();
        q.send(Msg::Rules(rules(1)));
        q.signal(
            1,
            Signal::Fire {
                binding: 1,
                kind: FireKind::Press,
            },
        );
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            Control::Engine {
                op: EngineOp::Disable,
                rule: "off-it".into()
            }
        );
        q.signal(
            1,
            Signal::Skipped {
                binding: 0,
                code: Skip::PasswordField,
            },
        );
        q.signal(1, Signal::RateLimited { binding: 0 });
        // A signal from an older table is dropped.
        q.signal(
            0,
            Signal::Fire {
                binding: 0,
                kind: FireKind::Press,
            },
        );
        q.signal(1, Signal::Emergency);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            Control::Emergency
        );
        let a = w.activity.lock().unwrap().clone();
        let outcomes: Vec<(&str, &str)> = a
            .entries()
            .map(|e| (e.rule.as_str(), e.outcome.as_str()))
            .collect();
        assert_eq!(
            outcomes,
            [
                ("off-it", "ok"),
                ("pause-it", "skipped_password_field"),
                ("pause-it", "rate_limited")
            ]
        );
        // Entries hold IDs and codes only.
        let j = a.to_json(10).to_string();
        assert!(!j.contains("F13"), "{j}");
    }

    #[test]
    fn holds_fire_after_their_time_unless_cancelled() {
        let (w, rx, _) = worker();
        let q = w.queue();
        q.send(Msg::Rules(rules(1)));
        q.signal(
            1,
            Signal::HoldArmed {
                binding: 0,
                hold_ms: 200,
            },
        );
        q.signal(1, Signal::HoldCancelled { binding: 0 });
        assert!(
            rx.recv_timeout(Duration::from_millis(400)).is_err(),
            "cancelled before 200 ms"
        );
        let t = Instant::now();
        q.signal(
            1,
            Signal::HoldArmed {
                binding: 0,
                hold_ms: 200,
            },
        );
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            Control::Engine {
                op: EngineOp::Pause,
                rule: "pause-it".into()
            }
        );
        assert!(
            t.elapsed() >= Duration::from_millis(190),
            "{:?}",
            t.elapsed()
        );
    }

    #[test]
    fn an_idle_worker_does_not_wake_up() {
        let (w, _rx, stats) = worker();
        w.queue().send(Msg::Rules(rules(1)));
        std::thread::sleep(Duration::from_millis(100));
        let before = stats.worker_wakeups.load(Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(
            stats.worker_wakeups.load(Ordering::Relaxed),
            before,
            "no timer while idle"
        );
    }

    #[test]
    fn a_full_queue_drops_and_counts_instead_of_blocking() {
        let stats = Arc::new(Stats::default());
        let (tx, _rx) = sync_channel::<Msg>(2);
        let q = Queue {
            tx,
            stats: Arc::clone(&stats),
            runs: Arc::default(),
        };
        let t = Instant::now();
        for _ in 0..10 {
            q.signal(1, Signal::Emergency);
        }
        assert!(t.elapsed() < Duration::from_millis(100));
        assert_eq!(stats.dropped.load(Ordering::Relaxed), 8);
    }

    #[test]
    fn activity_is_capped() {
        let mut a = Activity::default();
        for i in 0..(ACTIVITY_MAX + 5) {
            a.push(Entry {
                at: String::new(),
                rule: format!("r{i}"),
                profile: String::new(),
                mode: String::new(),
                outcome: "ok".into(),
            });
        }
        assert_eq!(a.len(), ACTIVITY_MAX);
        assert_eq!(a.entries().next().unwrap().rule, "r5");
        assert_eq!(a.to_json(3).as_array().unwrap().len(), 3);
    }

    // ---- Step 68: the worker carries out remap instructions (DDALGAK-SEC-005).

    use crate::inject::{Button, Recorder, SharedRecorder};

    const LCTRL: OutCode = OutCode::Vk(0xA2);
    const LSHIFT: OutCode = OutCode::Vk(0xA0);
    const F16: OutCode = OutCode::Vk(0x7F);

    fn ev(code: OutCode, up: bool) -> OutEvent {
        OutEvent { code, up }
    }

    #[test]
    fn remap_instructions_become_exactly_these_events() {
        let mut inj = Injector::new(Recorder::default());
        // Ctrl+Shift+J → Down: release the user's Ctrl and Shift (not recorded), press Down.
        let down = OutCode::Vk(0x28);
        execute(
            &mut inj,
            RemapOp::Press {
                lift: ModKey::LCtrl.bit() | ModKey::LShift.bit(),
                mods: 0,
                key: Some(down),
            },
        );
        assert_eq!(inj.held(), &[down], "only Down is Ddalgak's");
        execute(&mut inj, RemapOp::Repeat { key: down });
        execute(
            &mut inj,
            RemapOp::Release {
                key: Some(down),
                mods: 0,
                restore: ModKey::LCtrl.bit() | ModKey::LShift.bit(),
            },
        );
        assert!(inj.held().is_empty());
        assert_eq!(
            inj.backend().sent,
            [
                ev(LSHIFT, true),
                ev(LCTRL, true),
                ev(down, false),
                ev(down, false),
                ev(down, true),
                ev(LCTRL, false),
                ev(LSHIFT, false),
            ]
        );
        // A repeat or a release of what the ledger no longer holds sends nothing (a release-all
        // came first): no key is pressed again after it was let go.
        let mut inj = Injector::new(Recorder::default());
        execute(&mut inj, RemapOp::Repeat { key: F16 });
        execute(
            &mut inj,
            RemapOp::Release {
                key: Some(F16),
                mods: ModKey::LCtrl.bit(),
                restore: 0,
            },
        );
        assert!(inj.backend().sent.is_empty());
        // A tap: modifiers around the key, in one call.
        execute(
            &mut inj,
            RemapOp::Tap {
                lift: 0,
                mods: ModKey::LShift.bit(),
                key: Some(OutCode::Button(Button::Middle)),
                restore: 0,
            },
        );
        assert_eq!(
            inj.backend().sent,
            [
                ev(LSHIFT, false),
                ev(OutCode::Button(Button::Middle), false),
                ev(OutCode::Button(Button::Middle), true),
                ev(LSHIFT, true),
            ]
        );
        assert!(inj.held().is_empty(), "a tap leaves nothing held");
    }

    fn output_worker() -> (Worker, Arc<Mutex<Vec<OutEvent>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let w = Worker::start(
            Arc::new(Stats::default()),
            Box::new(|_| {}),
            Arc::new(Mutex::new(Activity::default())),
            Box::new(SharedRecorder(Arc::clone(&log))),
            Env::inert(),
        );
        (w, log)
    }

    fn wait_for(log: &Arc<Mutex<Vec<OutEvent>>>, n: usize) -> Vec<OutEvent> {
        let t = Instant::now();
        while log.lock().unwrap().len() < n && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        log.lock().unwrap().clone()
    }

    fn remap_rules(generation: u32) -> Rules {
        let mut r = rules(generation);
        let b = Arc::make_mut(&mut r.bindings);
        b[0].rule = "f15-f16".into();
        r
    }

    #[test]
    fn held_outputs_are_released_on_cancel_release_all_emergency_and_stop() {
        let (w, log) = output_worker();
        let q = w.queue();
        q.send(Msg::Rules(remap_rules(1)));
        let press = |key| Signal::Remap {
            binding: 0,
            op: RemapOp::Press {
                lift: 0,
                mods: 0,
                key: Some(key),
            },
        };
        q.signal(1, press(F16));
        q.send(Msg::Cancel); // pause, stop, session lock
        assert_eq!(wait_for(&log, 2), [ev(F16, false), ev(F16, true)]);
        q.signal(1, press(F16));
        q.signal(1, Signal::ReleaseAll); // desktop switch, elevated window
        assert_eq!(wait_for(&log, 4)[2..], [ev(F16, false), ev(F16, true)]);
        q.signal(1, press(F16));
        q.signal(1, Signal::Emergency);
        assert_eq!(wait_for(&log, 6)[4..], [ev(F16, false), ev(F16, true)]);
        // A held-back key is sent on and released like any other output.
        let c = OutCode::Exact {
            vk: 0x43,
            scan: 0x2E,
            ext: false,
        };
        q.signal(1, Signal::Replay { code: c, up: false });
        q.signal(
            1,
            Signal::Restore {
                mods: ModKey::LShift.bit(),
            },
        );
        assert_eq!(wait_for(&log, 8)[6..], [ev(c, false), ev(LSHIFT, false)]);
        drop(q);
        drop(w); // stop: the injector's Drop releases what is still held
        let all = log.lock().unwrap().clone();
        assert_eq!(all.last(), Some(&ev(c, true)), "{all:?}");
        assert!(
            !all.iter().any(|e| *e == ev(LSHIFT, true)),
            "the user's Shift is never released by a stop"
        );
    }

    #[test]
    fn a_reload_never_strands_a_held_output() {
        let (w, log) = output_worker();
        let q = w.queue();
        q.send(Msg::Rules(remap_rules(1)));
        q.signal(
            1,
            Signal::Remap {
                binding: 0,
                op: RemapOp::Press {
                    lift: 0,
                    mods: 0,
                    key: Some(F16),
                },
            },
        );
        // The rules change while F16 is held; the release comes with the new generation and a
        // binding index that means something else now: it still releases F16.
        q.send(Msg::Rules(rules(2)));
        q.signal(
            2,
            Signal::Remap {
                binding: 1,
                op: RemapOp::Release {
                    key: Some(F16),
                    mods: 0,
                    restore: 0,
                },
            },
        );
        assert_eq!(wait_for(&log, 2), [ev(F16, false), ev(F16, true)]);
        let a: Vec<(String, String)> = w
            .activity
            .lock()
            .unwrap()
            .entries()
            .map(|e| (e.rule.clone(), e.outcome.clone()))
            .collect();
        assert_eq!(
            a,
            [("f15-f16".to_string(), "ok".to_string())],
            "one entry per press"
        );
    }
}
