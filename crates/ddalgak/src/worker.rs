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
use crate::launch;
use crate::run::{self, Env, HotstringFire, Next, Run, RunShared};
use crate::stats::Stats;
use crate::table::{Action, Binding, EngineOp};
use crate::vk::ModKey;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
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
                    gate: Arc::new(Mutex::new(launch::Gate::default())),
                    epoch: Arc::new(AtomicU32::new(0)),
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

/// What the loop's typing runs and launches work with.
struct Ctx<'a> {
    inj: &'a mut Inj,
    env: &'a mut Env,
    shared: &'a RunShared,
    activity: &'a Arc<Mutex<Activity>>,
    active: Option<Active>,
    /// Launch rules running now and their last starts (`busy`, `rate_limited`; Step 70).
    gate: Arc<Mutex<launch::Gate>>,
    /// Bumped by every cancellation; a launch thread stops before its next target when it
    /// changed since the trigger fired.
    epoch: Arc<AtomicU32>,
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

    /// Starts a launch on its own short-lived thread (Step 70), so a slow shell hand-off never
    /// delays remaps or typing. One run per rule at a time (`busy`) and one start per rule per
    /// [`launch::DEBOUNCE`] (`rate_limited`). The thread records the outcome (codes only).
    fn launch(&mut self, r: &Rules, b: &Binding, plan: &Arc<launch::Plan>) {
        let entry = |outcome: &str| Entry {
            at: crate::time::now_rfc3339(),
            rule: b.rule.clone(),
            profile: r.profile.clone(),
            mode: r.mode.clone(),
            outcome: outcome.to_string(),
        };
        let admitted = self
            .gate
            .lock()
            .map_or(Err("busy"), |mut g| g.admit(&b.rule, Instant::now()));
        if let Err(code) = admitted {
            if let Ok(mut a) = self.activity.lock() {
                a.push(entry(code));
            }
            return;
        }
        let launcher = Arc::clone(&self.env.launcher);
        let plan = Arc::clone(plan);
        let gate = Arc::clone(&self.gate);
        let activity = Arc::clone(self.activity);
        let epoch = Arc::clone(&self.epoch);
        let fired = epoch.load(Ordering::SeqCst);
        let rule = b.rule.clone();
        let mut done = entry("ok");
        let spawned = std::thread::Builder::new()
            .name("ddalgak-launch".into())
            .spawn(move || {
                let cancelled = || epoch.load(Ordering::SeqCst) != fired;
                let code = launch::execute(
                    &plan,
                    &launch::Exec {
                        launcher: &*launcher,
                        var: &launch::process_var,
                        cancelled: &cancelled,
                    },
                );
                if let Ok(mut g) = gate.lock() {
                    g.done(&rule);
                }
                done.at = crate::time::now_rfc3339();
                done.outcome = code.to_string();
                if let Ok(mut a) = activity.lock() {
                    a.push(done);
                }
            });
        if spawned.is_err() {
            if let Ok(mut g) = self.gate.lock() {
                g.done(&b.rule);
            }
            if let Ok(mut a) = self.activity.lock() {
                a.push(entry("launch_failed"));
            }
        }
    }

    /// Starts a typing run for a fired rule, or drops the trigger with `busy`.
    fn start(&mut self, r: &Rules, b: &Binding, hs: Option<HotstringFire>) {
        if let Action::Launch(plan) = &b.action {
            self.launch(r, b, plan);
            return;
        }
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

    /// Ends the run in progress (pause, stop, emergency, reload); a launch still running stops
    /// before its next target (a program already started is never stopped).
    fn cancel(&mut self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
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
        // Typing runs and launches record their own outcome when they end.
        Action::Type(_) | Action::Keys(_) | Action::Launch(_) => return true,
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

    // ---- Step 69: typing runs on the worker (fake backend, fake probe, no clipboard).

    use crate::run::{HotstringFire, KeysSpec, TextSpec};
    use crate::text::{self, CaseForm};

    fn typing_rules(generation: u32, text: &str, key_delay_ms: u32) -> Rules {
        let b = |id: &str, action| Binding {
            rule: id.into(),
            trigger: Trigger::parse("F13").unwrap(),
            on: On::Press,
            repeat: false,
            pass_through: false,
            injected: Injected::Ignore,
            widen: false,
            injects: true,
            action,
            hotstring: None,
        };
        Rules {
            generation,
            bindings: Arc::new(vec![
                b(
                    "sign",
                    Action::Type(Arc::new(TextSpec {
                        text: text.into(),
                        paste: false,
                        key_delay_ms,
                        variables: false,
                    })),
                ),
                b(
                    "copy",
                    Action::Keys(Arc::new(KeysSpec {
                        chords: vec![crate::remap::Output::parse("Ctrl+C").unwrap()],
                        step_delay_ms: 0,
                    })),
                ),
            ]),
            profile: "default".into(),
            mode: "base".into(),
        }
    }

    fn outcomes(w: &Worker) -> Vec<(String, String)> {
        w.activity
            .lock()
            .unwrap()
            .entries()
            .map(|e| (e.rule.clone(), e.outcome.clone()))
            .collect()
    }

    fn wait_outcomes(w: &Worker, n: usize) -> Vec<(String, String)> {
        let t = Instant::now();
        while outcomes(w).len() < n && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        outcomes(w)
    }

    fn fire(binding: u16) -> Signal {
        Signal::Fire {
            binding,
            kind: FireKind::Press,
        }
    }

    #[test]
    fn a_typing_rule_types_exactly_its_text_and_records_only_its_outcome() {
        let (w, log) = output_worker();
        let q = w.queue();
        q.send(Msg::Rules(typing_rules(1, "감사합니다 👍\n", 0)));
        q.signal(1, fire(0));
        assert_eq!(
            wait_outcomes(&w, 1),
            [("sign".to_string(), "ok".to_string())]
        );
        assert_eq!(*log.lock().unwrap(), text::events("감사합니다 👍\n"));
        let j = w.activity.lock().unwrap().to_json(10).to_string();
        assert!(!j.contains("감사"), "activity never holds the text: {j}");
        // send_keys taps its chord.
        q.signal(1, fire(1));
        assert_eq!(
            wait_outcomes(&w, 2)[1],
            ("copy".to_string(), "ok".to_string())
        );
        let all = log.lock().unwrap().clone();
        assert_eq!(
            all[all.len() - 4..],
            run::chord_events(&crate::remap::Output::parse("Ctrl+C").unwrap())[..]
        );
    }

    fn hotstring_fire(typed: u32) -> HotstringFire {
        HotstringFire {
            erase: 3,
            case: CaseForm::AsWritten,
            key: OutCode::Exact {
                vk: 0x20,
                scan: 0x39,
                ext: false,
            },
            mods: 0,
            retype: true,
            consumed: true,
            typed,
        }
    }

    #[test]
    fn a_trigger_while_typing_is_busy_and_a_hotstring_gets_its_key_back() {
        let (w, log) = output_worker();
        let q = w.queue();
        q.send(Msg::Rules(typing_rules(1, "abcdefgh", 30)));
        q.signal(1, fire(0));
        wait_for(&log, 2);
        q.signal(1, fire(1)); // another typing rule
        q.signal(
            1,
            Signal::Hotstring {
                binding: 0,
                fire: hotstring_fire(0),
            },
        );
        let got = wait_outcomes(&w, 3);
        assert_eq!(
            got,
            [
                ("copy".to_string(), "busy".to_string()),
                ("sign".to_string(), "busy".to_string()),
                ("sign".to_string(), "ok".to_string()),
            ]
        );
        let all = log.lock().unwrap().clone();
        let key = OutCode::Exact {
            vk: 0x20,
            scan: 0x39,
            ext: false,
        };
        let back = all.iter().position(|e| *e == OutEvent::down(key)).unwrap();
        assert_eq!(all[back + 1], OutEvent::up(key), "given back unchanged");
        let typed: Vec<OutEvent> = all.iter().filter(|e| e.code != key).copied().collect();
        assert_eq!(
            typed,
            text::events("abcdefgh"),
            "the run itself was not disturbed"
        );
    }

    #[test]
    fn pause_emergency_and_reload_cancel_a_run_at_its_next_piece() {
        for how in ["pause", "emergency", "reload"] {
            let (tx, rx) = channel();
            let tx = Mutex::new(tx);
            let log = Arc::new(Mutex::new(Vec::new()));
            let w = Worker::start(
                Arc::new(Stats::default()),
                Box::new(move |c| {
                    let _ = tx.lock().unwrap().send(c);
                }),
                Arc::new(Mutex::new(Activity::default())),
                Box::new(SharedRecorder(Arc::clone(&log))),
                Env::inert(),
            );
            let q = w.queue();
            q.send(Msg::Rules(typing_rules(1, &"x".repeat(200), 20)));
            q.signal(1, fire(0));
            wait_for(&log, 6);
            match how {
                "pause" => q.send(Msg::Cancel),
                "emergency" => q.signal(1, Signal::Emergency),
                _ => q.send(Msg::Rules(typing_rules(2, "y", 0))),
            }
            assert_eq!(
                wait_outcomes(&w, 1),
                [("sign".to_string(), "cancelled".to_string())],
                "{how}"
            );
            let n = log.lock().unwrap().len();
            std::thread::sleep(Duration::from_millis(100));
            assert_eq!(log.lock().unwrap().len(), n, "{how}: nothing more is typed");
            assert!(n < 2 * 200, "{how}: stopped early ({n} events)");
            if how == "emergency" {
                assert_eq!(
                    rx.recv_timeout(Duration::from_secs(5)).unwrap(),
                    Control::Emergency
                );
            }
        }
    }

    #[test]
    fn a_hotstring_from_an_older_table_only_gets_its_key_back() {
        let (w, log) = output_worker();
        let q = w.queue();
        q.send(Msg::Rules(typing_rules(2, "by the way", 0)));
        q.signal(
            1,
            Signal::Hotstring {
                binding: 0,
                fire: hotstring_fire(0),
            },
        );
        let got = wait_for(&log, 2);
        assert_eq!(got.len(), 2);
        assert!(outcomes(&w).is_empty(), "nothing ran");
        // A current one erases, types and types the end key again.
        q.signal(
            2,
            Signal::Hotstring {
                binding: 0,
                fire: hotstring_fire(0),
            },
        );
        assert_eq!(wait_outcomes(&w, 1)[0].1, "ok");
        let all = log.lock().unwrap().clone();
        let bs = all
            .iter()
            .filter(|e| **e == OutEvent::down(OutCode::Vk(text::VK_BACK)))
            .count();
        assert_eq!(bs, 3);
    }

    // Launches (Step 70): the real worker thread with a fake launcher that records every call.
    use crate::launch::{self as launch_mod, Call, FakeLauncher, ShellRequest, Show, Target};

    fn launch_rules(generation: u32, urls: &[&str]) -> Rules {
        let b = |id: &str, url: &str| Binding {
            rule: id.into(),
            trigger: Trigger::parse("F13").unwrap(),
            on: On::Press,
            repeat: false,
            pass_through: false,
            injected: Injected::Ignore,
            widen: false,
            injects: false,
            action: Action::Launch(Arc::new(launch_mod::Plan {
                targets: vec![Target::Url(url.into())],
                stop_on_error: true,
            })),
            hotstring: None,
        };
        Rules {
            generation,
            bindings: Arc::new(
                urls.iter()
                    .enumerate()
                    .map(|(i, u)| b(&format!("open-{i}"), u))
                    .collect(),
            ),
            profile: "default".into(),
            mode: "base".into(),
        }
    }

    fn launch_worker(f: &Arc<FakeLauncher>) -> Worker {
        let env = Env {
            launcher: Arc::clone(f) as Arc<dyn launch_mod::Launcher>,
            ..Env::inert()
        };
        Worker::start(
            Arc::new(Stats::default()),
            Box::new(|_| {}),
            Arc::new(Mutex::new(Activity::default())),
            Box::new(Recorder::default()),
            env,
        )
    }

    #[test]
    fn a_launch_rule_opens_its_url_once_and_records_only_its_outcome() {
        let f = Arc::new(FakeLauncher::default());
        let w = launch_worker(&f);
        let q = w.queue();
        q.send(Msg::Rules(launch_rules(
            1,
            &["https://example.com/reset?token=s3cret"],
        )));
        q.signal(1, fire(0));
        assert_eq!(wait_outcomes(&w, 1), [("open-0".into(), "ok".into())]);
        assert_eq!(
            f.calls(),
            [Call::Shell(ShellRequest {
                verb: "open",
                target: "https://example.com/reset?token=s3cret".into(),
                show: Show::Normal
            })]
        );
        let j = w.activity.lock().unwrap().to_json(10).to_string();
        assert!(
            !j.contains("example") && !j.contains("s3cret"),
            "activity holds codes only: {j}"
        );
    }

    #[test]
    fn rapid_triggers_are_busy_while_running_and_rate_limited_after() {
        let f = Arc::new(FakeLauncher::default());
        // A slow shell hand-off: the second press arrives while the first launch still runs.
        *f.delay.lock().unwrap() = Duration::from_millis(300);
        let w = launch_worker(&f);
        let q = w.queue();
        q.send(Msg::Rules(launch_rules(
            1,
            &["https://example.com/", "https://example.org/"],
        )));
        q.signal(1, fire(0));
        q.signal(1, fire(0));
        // Another launch rule is not held up by the first one, nor are other messages.
        q.signal(1, fire(1));
        let out = wait_outcomes(&w, 3);
        assert!(out.contains(&("open-0".into(), "busy".into())), "{out:?}");
        assert!(out.contains(&("open-0".into(), "ok".into())), "{out:?}");
        assert!(out.contains(&("open-1".into(), "ok".into())), "{out:?}");
        // Finished, but within a second of its start: a double press opens nothing more.
        *f.delay.lock().unwrap() = Duration::ZERO;
        q.signal(1, fire(0));
        let out = wait_outcomes(&w, 4);
        assert_eq!(out[3], ("open-0".into(), "rate_limited".into()));
        assert_eq!(f.calls().len(), 2, "one launch per rule");
        std::thread::sleep(launch_mod::DEBOUNCE);
        q.signal(1, fire(0));
        assert_eq!(wait_outcomes(&w, 5)[4], ("open-0".into(), "ok".into()));
        assert_eq!(f.calls().len(), 3);
    }

    #[test]
    fn a_pause_between_targets_ends_a_launch_run_before_its_next_target() {
        let f = Arc::new(FakeLauncher::default());
        *f.delay.lock().unwrap() = Duration::from_millis(300);
        let w = launch_worker(&f);
        let q = w.queue();
        let mut r = launch_rules(1, &["https://example.com/"]);
        r.bindings = Arc::new(vec![Binding {
            action: Action::Launch(Arc::new(launch_mod::Plan {
                targets: vec![
                    Target::Url("https://example.com/".into()),
                    Target::Url("https://example.org/".into()),
                ],
                stop_on_error: true,
            })),
            ..r.bindings[0].clone()
        }]);
        q.send(Msg::Rules(r));
        q.signal(1, fire(0));
        std::thread::sleep(Duration::from_millis(100));
        q.send(Msg::Cancel);
        assert_eq!(
            wait_outcomes(&w, 1),
            [("open-0".into(), "cancelled".into())]
        );
        assert_eq!(f.calls().len(), 1, "the second target never opened");
    }

    #[test]
    fn loading_and_reloading_launch_rules_starts_nothing_until_a_trigger() {
        // Rules arrive (engine start, a reload, an import that was then enabled): nothing opens
        // until the user's trigger fires.
        let f = Arc::new(FakeLauncher::default());
        let w = launch_worker(&f);
        let q = w.queue();
        q.send(Msg::Rules(launch_rules(1, &["https://example.com/"])));
        q.send(Msg::Rules(launch_rules(
            2,
            &["https://example.com/", "https://example.org/"],
        )));
        q.send(Msg::Cancel);
        // An old table's trigger after a reload starts nothing either.
        q.signal(1, fire(0));
        std::thread::sleep(Duration::from_millis(200));
        assert!(f.calls().is_empty(), "{:?}", f.calls());
        assert!(outcomes(&w).is_empty());
    }

    #[test]
    fn the_inert_environment_never_starts_anything() {
        // Tests and the debug test host without a launch root: every launch is refused.
        let w = Worker::start(
            Arc::new(Stats::default()),
            Box::new(|_| {}),
            Arc::new(Mutex::new(Activity::default())),
            Box::new(Recorder::default()),
            Env::inert(),
        );
        let q = w.queue();
        q.send(Msg::Rules(launch_rules(1, &["https://example.com/"])));
        q.signal(1, fire(0));
        assert_eq!(
            wait_outcomes(&w, 1),
            [("open-0".into(), "launch_failed".into())]
        );
    }
}
